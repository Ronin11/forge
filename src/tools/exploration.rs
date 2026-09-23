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
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(events: Vec<Value>, changed: &[String]) -> Option<Measures> {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("attempt.jsonl");
        std::fs::write(
            &log,
            events
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        measure(&log, "/wt", changed)
    }

    fn call(id: &str, message: &str, name: &str, input: Value) -> Value {
        json!({"type":"assistant","message":{"id":message,"content":[{"type":"tool_use","id":id,"name":name,"input":input}]}})
    }

    fn result(id: &str, text: &str) -> Value {
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":text}]}})
    }

    #[test]
    fn measures_chains_returned_characters_and_turns_without_replay_double_counts() {
        let read = call(
            "r",
            "m1",
            "Read",
            json!({"file_path":"/wt/a.rs","offset":3,"limit":2}),
        );
        let events = vec![
            call("g", "m0", "Grep", json!({"pattern":"foo"})),
            read.clone(),
            read,
            result("r", "héllo"),
            result("r", "replayed"),
            call("r2", "m1", "Read", json!({"file_path":"/wt/b.rs"})),
            result("r2", "ignore this"),
            call("r3", "m1", "Read", json!({"file_path":"/wt/c.rs"})),
            result("r3", "git changed"),
            call("e", "m2", "Edit", json!({"file_path":"/wt/b.rs"})),
            call(
                "nav",
                "m3",
                "Bash",
                json!({"command":"forge-repomap outline a.rs; forge-repomap def foo"}),
            ),
        ];
        assert_eq!(
            run(events, &["c.rs".into()]).unwrap(),
            Measures {
                grep_then_ranged_read_chains: 1,
                unedited_read_chars: 5,
                turns_before_first_edit: Some(2),
                outline_calls: 1,
                def_calls: 1,
            }
        );
    }

    #[test]
    fn codex_completed_items_pair_reads_and_count_navigation_once() {
        let events = vec![
            json!({"type":"turn.started"}),
            json!({"type":"item.completed","item":{"id":"g","type":"command_execution","command":"rg foo a.rs","exit_code":0}}),
            json!({"type":"item.started","item":{"id":"r","type":"command_execution","command":"/bin/bash -lc \"sed -n '1,3p' a.rs\""}}),
            json!({"type":"item.completed","item":{"id":"r","type":"command_execution","command":"/bin/bash -lc \"sed -n '1,3p' a.rs\"","aggregated_output":"αβ","exit_code":0}}),
            json!({"type":"item.started","item":{"id":"n","type":"command_execution","command":"forge-repomap def foo"}}),
            json!({"type":"item.completed","item":{"id":"n","type":"command_execution","command":"forge-repomap def foo","exit_code":0}}),
            json!({"type":"turn.started"}),
            json!({"type":"item.completed","item":{"id":"e","type":"file_change","changes":[{"path":"other.rs"}]}}),
        ];
        assert_eq!(
            run(events, &[]).unwrap(),
            Measures {
                grep_then_ranged_read_chains: 1,
                unedited_read_chars: 2,
                turns_before_first_edit: Some(1),
                outline_calls: 0,
                def_calls: 1,
            }
        );
    }

    #[test]
    fn unavailable_and_no_edit_are_not_fabricated_zeroes() {
        assert_eq!(run(vec![json!({"type":"forge_prompt"})], &[]), None);
        let mut error = result("r", "failure text");
        error["message"]["content"][0]["is_error"] = json!(true);
        let m = run(
            vec![
                call("r", "m", "Read", json!({"file_path":"/wt/a.rs"})),
                error,
            ],
            &[],
        )
        .unwrap();
        assert_eq!(m, Measures::default());
        assert_eq!(shell("echo forge-repomap def foo").def, 0);
        assert!(shell("cat a.rs b.rs").read.is_none());
    }
}
