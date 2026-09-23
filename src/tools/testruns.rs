//! Test runs in an attempt's stream: how often the model went through
//! `forge-test`, how often the cache answered, what bypassed it, and how much
//! wall time tests took. Counted from the log, never inferred.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRuns {
    /// `forge-test` invocations (not `forge-test failing`).
    pub forge_test_calls: u64,
    /// Of those, answered from the cache ("cached: tree unchanged").
    pub cache_hits: u64,
    /// Raw test commands (`cargo test`, `npm test`, ...) that skipped it.
    pub raw_commands: u64,
    /// Runs of the whole suite, through `forge-test` or raw.
    pub full_suite_runs: u64,
    /// Runs after the first with no edit since the previous run.
    pub runs_without_edit: u64,
    /// Milliseconds from call to result over the calls that ran tests.
    pub wall_ms: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    ForgeTest { full: bool },
    Raw { full: bool },
}

fn words(segment: &str) -> Vec<&str> {
    segment
        .split_whitespace()
        .map(|w| w.trim_matches(['\'', '"', '(', ')']))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether the arguments name nothing to narrow the run: flags only, or
/// a workspace-wide path.
fn whole(args: &[&str]) -> bool {
    args.iter()
        .all(|a| a.starts_with('-') || matches!(*a, "./..." | "." | "tests"))
        && !args.contains(&"--")
}

fn segment_kind(segment: &str) -> Option<Kind> {
    let mut w = words(segment);
    while w
        .first()
        .is_some_and(|x| x.contains('=') && !x.starts_with('-'))
    {
        w.remove(0);
    }
    let program = w.first()?.rsplit('/').next()?;
    let rest = &w[1..];
    match program {
        "forge-test" => {
            if rest.first() == Some(&"failing") {
                return None;
            }
            let rest: Vec<&str> = rest
                .iter()
                .copied()
                .skip_while(|a| matches!(*a, "--fresh" | "--"))
                .collect();
            Some(Kind::ForgeTest {
                full: rest.is_empty(),
            })
        }
        "cargo" if matches!(rest.first(), Some(&"test") | Some(&"nextest")) => {
            let rest = if rest.get(1) == Some(&"run") {
                &rest[2..]
            } else {
                &rest[1..]
            };
            Some(Kind::Raw { full: whole(rest) })
        }
        "npm" | "yarn" | "pnpm" if rest.first() == Some(&"test") => Some(Kind::Raw {
            full: rest.len() == 1,
        }),
        "npx" if matches!(rest.first(), Some(&"vitest") | Some(&"jest")) => Some(Kind::Raw {
            full: whole(&rest[1..]) || rest.get(1) == Some(&"run") && whole(&rest[2..]),
        }),
        "pytest" | "py.test" => Some(Kind::Raw { full: whole(rest) }),
        "go" if rest.first() == Some(&"test") => Some(Kind::Raw {
            full: whole(&rest[1..]),
        }),
        _ => None,
    }
}

fn kinds(command: &str) -> Vec<Kind> {
    let command = command.trim();
    for prefix in [
        "bash -c ",
        "bash -lc ",
        "sh -c ",
        "/bin/bash -lc ",
        "/bin/sh -c ",
    ] {
        if let Some(inner) = command.strip_prefix(prefix) {
            return kinds(inner.trim_matches(['\'', '"']));
        }
    }
    command
        .split([';', '\n', '&', '|'])
        .filter_map(segment_kind)
        .collect()
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Read one attempt's stream. `None` when the log cannot be read.
pub fn measure(log: &Path) -> Option<TestRuns> {
    let text = std::fs::read_to_string(log).ok()?;
    let mut out = TestRuns::default();
    let mut seen = BTreeSet::new();
    // id -> (started_ms, forge-test calls in it)
    let mut open: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut last_ms = 0u64;
    let mut ran = false;
    let mut edited = false;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ms = v["forge_ms"].as_u64().unwrap_or(last_ms);
        last_ms = ms;
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("tool_use") => {
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    match b["name"].as_str().unwrap_or("") {
                        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => edited = true,
                        "Bash" => {
                            let ks = kinds(b["input"]["command"].as_str().unwrap_or(""));
                            for k in &ks {
                                if ran && !edited {
                                    out.runs_without_edit += 1;
                                }
                                ran = true;
                                edited = false;
                                let full = match k {
                                    Kind::ForgeTest { full } => {
                                        out.forge_test_calls += 1;
                                        *full
                                    }
                                    Kind::Raw { full } => {
                                        out.raw_commands += 1;
                                        *full
                                    }
                                };
                                if full {
                                    out.full_suite_runs += 1;
                                }
                            }
                            if !ks.is_empty() {
                                let ft = ks
                                    .iter()
                                    .filter(|k| matches!(k, Kind::ForgeTest { .. }))
                                    .count() as u64;
                                open.insert(id, (ms, ft));
                            }
                        }
                        _ => {}
                    }
                }
                Some("tool_result") => {
                    let id = b["tool_use_id"].as_str().unwrap_or("");
                    if let Some((started, ft)) = open.remove(id) {
                        out.wall_ms += ms.saturating_sub(started);
                        if ft > 0 && text_of(&b["content"]).contains("cached: tree unchanged") {
                            out.cache_hits += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(id: &str, cmd: &str, ms: u64) -> String {
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":cmd}}]},"forge_ms":ms}).to_string()
    }
    fn edit(id: &str, ms: u64) -> String {
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":id,"name":"Edit","input":{"file_path":"/w/a"}}]},"forge_ms":ms}).to_string()
    }
    fn result(id: &str, text: &str, ms: u64) -> String {
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"content":text}]},"forge_ms":ms}).to_string()
    }

    #[test]
    fn a_stream_counts_runs_hits_bypasses_and_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("a.jsonl");
        let lines = [
            bash("1", "forge-test", 0),
            result("1", "$ cargo test\nexit 0", 4000),
            bash("2", "forge-test", 4100),
            result("2", "cached: tree unchanged since x\n$ cargo test", 4200),
            edit("3", 4300),
            bash("4", "cd /w && cargo test foo", 4400),
            result("4", "ok", 5400),
            bash("5", "forge-test failing < x", 5500),
            bash("6", "forge-test --fresh -- cargo test foo", 5600),
            result("6", "ok", 5700),
        ];
        std::fs::write(&log, lines.join("\n")).unwrap();
        let t = measure(&log).unwrap();
        assert_eq!(t.forge_test_calls, 3);
        assert_eq!(t.cache_hits, 1);
        assert_eq!(t.raw_commands, 1);
        assert_eq!(t.full_suite_runs, 2, "forge-test twice with no arguments");
        assert_eq!(t.runs_without_edit, 2, "the second, and the sixth");
        assert_eq!(t.wall_ms, 4000 + 100 + 1000 + 100);
    }

    #[test]
    fn full_suite_needs_no_filter() {
        assert!(matches!(
            segment_kind("cargo test --workspace"),
            Some(Kind::Raw { full: true })
        ));
        assert!(matches!(
            segment_kind("cargo test foo"),
            Some(Kind::Raw { full: false })
        ));
        assert!(matches!(
            segment_kind("npm test"),
            Some(Kind::Raw { full: true })
        ));
        assert!(segment_kind("ls").is_none());
    }
}
