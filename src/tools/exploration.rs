//! Observable navigation in attempt logs. Never infer returned text from today's files.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measures {
    pub grep_then_ranged_read_chains: u64,
    pub unedited_read_chars: u64,
    pub turns_before_first_edit: Option<u64>,
    pub outline_calls: u64,
    pub def_calls: u64,
}

fn path(p: &str, root: &str) -> String {
    Path::new(p)
        .strip_prefix(root)
        .unwrap_or(Path::new(p))
        .to_string_lossy()
        .trim_start_matches("./")
        .to_string()
}

fn chars(v: &Value) -> u64 {
    if let Some(s) = v.as_str() {
        s.chars().count() as u64
    } else if let Some(a) = v.as_array() {
        a.iter()
            .filter_map(|b| b["text"].as_str())
            .map(|s| s.chars().count() as u64)
            .sum()
    } else {
        0
    }
}

#[derive(Default)]
struct Call {
    search: bool,
    read: Option<String>,
    ranged: bool,
    edits: Vec<String>,
    outline: u64,
    def: u64,
}

// Deliberately limited to identifiable shell commands. Arbitrary scripts and
// compound outputs cannot reliably be attributed to a single file.
fn shell(command: &str) -> Call {
    let command = command.trim();
    for prefix in [
        "bash -c ",
        "bash -lc ",
        "sh -c ",
        "/bin/bash -lc ",
        "/bin/sh -c ",
    ] {
        if let Some(inner) = command.strip_prefix(prefix) {
            return shell(inner.trim_matches(['\'', '"']));
        }
    }
    let mut call = Call::default();
    let segments: Vec<_> = command
        .split([';', '\n', '&', '|'])
        .filter(|s| !s.trim().is_empty())
        .collect();
    for segment in &segments {
        let words: Vec<_> = segment
            .split_whitespace()
            .map(|w| w.trim_matches(['\'', '"']))
            .collect();
        let Some(program) = words.first().and_then(|w| w.rsplit('/').next()) else {
            continue;
        };
        match program {
            "rg" | "grep" => call.search = true,
            "forge-repomap" => match words.get(1).copied() {
                Some("outline") => call.outline += 1,
                Some("def") => call.def += 1,
                _ => {}
            },
            "cat" if segments.len() == 1 && words.len() == 2 && !words[1].starts_with('-') => {
                call.read = Some(words[1].into());
            }
            "sed" if segments.len() == 1 && words.len() == 4 && words[1] == "-n" => {
                let range = words[2].trim_end_matches('p');
                if range
                    .split(',')
                    .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                    && words[2].ends_with('p')
                {
                    call.read = Some(words[3].into());
                    call.ranged = true;
                }
            }
            _ => {}
        }
    }
    call
}

pub fn measure(log: &Path, root: &str, changed: &[String]) -> Option<Measures> {
    let text = std::fs::read_to_string(log).ok()?;
    let mut out = Measures::default();
    let mut edited: BTreeSet<String> = changed.iter().map(|p| path(p, root)).collect();
    let mut reads: BTreeMap<String, u64> = BTreeMap::new();
    let mut pending: BTreeMap<String, String> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut messages = BTreeSet::new();
    let mut turns = 0u64;
    let mut previous_search = false;
    let mut observed = false;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = v["type"].as_str().unwrap_or("");
        if kind == "turn.started" {
            observed = true;
            turns += 1;
        }
        if kind == "assistant" {
            observed = true;
            if v["message"]["id"]
                .as_str()
                .is_none_or(|id| messages.insert(id.to_string()))
            {
                turns += 1;
            }
        }
        let mut calls: Vec<(String, Call)> = Vec::new();
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            if b["type"] == "tool_result" {
                if let Some(p) = b["tool_use_id"].as_str().and_then(|id| pending.remove(id))
                    && b["is_error"] != true
                {
                    *reads.entry(p).or_default() += chars(&b["content"]);
                }
            } else if b["type"] == "tool_use" {
                let input = &b["input"];
                let mut call = Call::default();
                match b["name"].as_str().unwrap_or("") {
                    "Bash" => call = shell(input["command"].as_str().unwrap_or("")),
                    "Grep" => call.search = true,
                    "Read" => {
                        call.read = input["file_path"].as_str().map(str::to_string);
                        call.ranged = input["offset"].is_number() || input["limit"].is_number();
                    }
                    "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                        if let Some(p) = input["file_path"]
                            .as_str()
                            .or(input["notebook_path"].as_str())
                        {
                            call.edits.push(p.into());
                        }
                    }
                    _ => {}
                }
                if let Some(id) = b["id"].as_str() {
                    calls.push((id.into(), call));
                }
            }
        }
        if kind == "item.started" || kind == "item.completed" {
            let item = &v["item"];
            let id = item["id"].as_str().unwrap_or("");
            if item["type"] == "command_execution" {
                observed = true;
                calls.push((id.into(), shell(item["command"].as_str().unwrap_or(""))));
            } else if item["type"] == "file_change" {
                observed = true;
                let edits = item["changes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["path"].as_str().map(str::to_string))
                    .collect();
                calls.push((
                    id.into(),
                    Call {
                        edits,
                        ..Default::default()
                    },
                ));
            }
        }
        for (id, call) in calls {
            if !seen.insert(id.clone()) {
                continue;
            }
            out.outline_calls += call.outline;
            out.def_calls += call.def;
            if previous_search && call.ranged {
                out.grep_then_ranged_read_chains += 1;
            }
            previous_search = call.search;
            if let Some(p) = call.read {
                pending.insert(id, path(&p, root));
            }
            if !call.edits.is_empty() {
                out.turns_before_first_edit
                    .get_or_insert(turns.saturating_sub(1));
                edited.extend(call.edits.iter().map(|p| path(p, root)));
            }
        }
        if kind == "item.completed" {
            let item = &v["item"];
            if let Some(p) = item["id"].as_str().and_then(|id| pending.remove(id))
                && item["exit_code"].as_i64() == Some(0)
            {
                *reads.entry(p).or_default() += chars(&item["aggregated_output"]);
            }
        }
    }
    out.unedited_read_chars = reads
        .iter()
        .filter(|(p, _)| !edited.contains(*p))
        .map(|(_, n)| n)
        .sum();
    observed.then_some(out)
}
