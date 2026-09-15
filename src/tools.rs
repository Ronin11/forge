//! What an attempt ran, read back from its stream: every tool call with
//! its duration to the matching result, shell commands by family, files
//! read. Facts for the audit and the cost anti-patterns; never opinions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct Use {
    pub calls: u64,
    /// Milliseconds from call to result, summed; calls with no result count 0.
    pub ms: u64,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct Tools {
    /// By tool name: Read, Edit, Bash, ...
    pub by_tool: BTreeMap<String, Use>,
    /// Shell commands by family: `npx vitest`, `git`, `cargo test`, ...
    pub shell: BTreeMap<String, Use>,
    /// Files read, by path relative to the worktree, with counts.
    pub reads: BTreeMap<String, u64>,
    /// Milliseconds from the first frame to the last.
    pub span_ms: u64,
    /// The last `RECENT_CALLS` tool calls, oldest first: what an attempt
    /// was doing right before it stopped, for a run that never reached a
    /// result frame.
    #[serde(default)]
    pub recent: Vec<String>,
}

/// How many of the most recent tool calls `summarize` keeps in `Tools::recent`.
const RECENT_CALLS: usize = 5;

/// The family of a shell command: its program, plus the subcommand for
/// the runners that have one (`npm run x`, `npx x`, `cargo x`, `git x`).
pub fn family(command: &str) -> String {
    let cmd = command.trim();
    let cmd = cmd
        .strip_prefix("cd ")
        .and_then(|rest| rest.split_once("&&").map(|(_, r)| r.trim()))
        .unwrap_or(cmd);

    // Handle bash -c and sh -c wrappers
    if let Some(rest) = cmd.strip_prefix("bash -c ") {
        let inner = rest.trim_matches(|c| c == '\'' || c == '"');
        return family(inner);
    }
    if let Some(rest) = cmd.strip_prefix("sh -c ") {
        let inner = rest.trim_matches(|c| c == '\'' || c == '"');
        return family(inner);
    }

    let mut words = cmd
        .split_whitespace()
        .filter(|w| !w.contains('=') || w.starts_with('-'));
    let Some(first) = words.next() else {
        return "?".into();
    };
    let first = first.trim_start_matches(['(', '{']);
    let first = first.rsplit('/').next().unwrap_or(first);
    match first {
        "npm" | "npx" | "pnpm" | "yarn" | "cargo" | "git" | "make" | "just" | "python"
        | "python3" | "node" | "go" | "uv" | "bun" => {
            let mut sub = words.next().unwrap_or("");
            if first == "npm" && sub == "run" {
                sub = words.next().unwrap_or("");
            }
            if sub.is_empty() || sub.starts_with('-') {
                first.to_string()
            } else {
                format!("{first} {sub}")
            }
        }
        _ => first.to_string(),
    }
}

/// Summarize an attempt's stream log.
pub fn summarize(log_path: &Path, worktree: &str) -> Option<Tools> {
    let text = std::fs::read_to_string(log_path).ok()?;
    let mut out = Tools::default();
    // id -> (name, family/path, started_ms)
    let mut open: BTreeMap<String, (String, Option<String>, u64)> = BTreeMap::new();
    let mut seen_ids = std::collections::HashSet::new();
    let (mut first_ms, mut last_ms): (Option<u64>, u64) = (None, 0);
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ms = v["forge_ms"].as_u64().unwrap_or(last_ms);
        if v["forge_ms"].is_u64() {
            first_ms.get_or_insert(ms);
            last_ms = ms;
        }
        let Some(blocks) = v["message"]["content"].as_array() else {
            continue;
        };
        for b in blocks {
            match b["type"].as_str() {
                Some("tool_use") => {
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    if !seen_ids.insert(id.clone()) {
                        continue;
                    }
                    let name = b["name"].as_str().unwrap_or("?").to_string();
                    let key = match name.as_str() {
                        "Bash" => b["input"]["command"].as_str().map(family),
                        "Read" => b["input"]["file_path"].as_str().map(|p| {
                            p.strip_prefix(worktree)
                                .map(|r| r.trim_start_matches('/').to_string())
                                .unwrap_or_else(|| p.to_string())
                        }),
                        _ => None,
                    };
                    out.by_tool.entry(name.clone()).or_default().calls += 1;
                    if name == "Bash"
                        && let Some(k) = &key
                    {
                        out.shell.entry(k.clone()).or_default().calls += 1;
                    }
                    if name == "Read"
                        && let Some(k) = &key
                    {
                        *out.reads.entry(k.clone()).or_default() += 1;
                    }
                    out.recent.push(match &key {
                        Some(k) => format!("{name}: {k}"),
                        None => name.clone(),
                    });
                    if out.recent.len() > RECENT_CALLS {
                        out.recent.remove(0);
                    }
                    open.insert(id, (name, key, ms));
                }
                Some("tool_result") => {
                    let id = b["tool_use_id"].as_str().unwrap_or("");
                    if let Some((name, key, started)) = open.remove(id) {
                        let took = ms.saturating_sub(started);
                        out.by_tool.entry(name.clone()).or_default().ms += took;
                        if name == "Bash"
                            && let Some(k) = key
                        {
                            out.shell.entry(k).or_default().ms += took;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out.span_ms = last_ms.saturating_sub(first_ms.unwrap_or(0));
    Some(out)
}

impl Tools {
    /// One line for a human: tools with time, then the shell families.
    pub fn line(&self) -> String {
        let mut parts: Vec<String> = self
            .by_tool
            .iter()
            .map(|(n, u)| format!("{n} {} ({:.1}s)", u.calls, u.ms as f64 / 1000.0))
            .collect();
        let shell: Vec<String> = self
            .shell
            .iter()
            .map(|(n, u)| format!("{n} {} ({:.1}s)", u.calls, u.ms as f64 / 1000.0))
            .collect();
        if !shell.is_empty() {
            parts.push(format!("shell: {}", shell.join(", ")));
        }
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_families_are_the_program_and_its_subcommand() {
        assert_eq!(family("npx vitest run tests/x.test.ts"), "npx vitest");
        assert_eq!(family("npm run typecheck"), "npm typecheck");
        assert_eq!(family("npm test"), "npm test");
        assert_eq!(family("cd /w && cargo test --workspace"), "cargo test");
        assert_eq!(family("git status --porcelain"), "git status");
        assert_eq!(family("FOO=1 ./scripts/check.sh"), "check.sh");
        assert_eq!(family("ls -la"), "ls");
        assert_eq!(family("(npm test 2>&1 | tail -5)"), "npm test");
        assert_eq!(family(""), "?");
    }

    #[test]
    fn bash_c_and_sh_c_wrappers_extract_inner_command() {
        assert_eq!(family("bash -c 'npm test'"), "npm test");
        assert_eq!(family("sh -c \"cargo test\""), "cargo test");
    }

    #[test]
    fn a_stream_summarizes_into_calls_durations_and_reads() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("1-1.jsonl");
        std::fs::write(&log, [
            r#"{"type":"forge_prompt","text":"x"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"Read","input":{"file_path":"/wt/src/a.ts"}}]},"forge_ms":100}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a"}]},"forge_ms":150}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"npx vitest run"}}]},"forge_ms":200}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"npx vitest run"}}]},"forge_ms":201}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b"}]},"forge_ms":1400}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c","name":"Read","input":{"file_path":"/wt/src/a.ts"}}]},"forge_ms":1500}"#,
            r#"{"type":"result","subtype":"success","forge_ms":1600}"#,
        ].join("\n")).unwrap();
        let t = summarize(&log, "/wt").unwrap();
        assert_eq!(
            t.by_tool["Read"],
            Use { calls: 2, ms: 50 },
            "a read with no result counts its call, not time"
        );
        assert_eq!(
            t.by_tool["Bash"],
            Use { calls: 1, ms: 1200 },
            "the repeated frame counts once"
        );
        assert_eq!(t.shell["npx vitest"], Use { calls: 1, ms: 1200 });
        assert_eq!(t.reads["src/a.ts"], 2);
        assert_eq!(t.span_ms, 1500);
        assert_eq!(
            t.recent,
            vec!["Read: src/a.ts", "Bash: npx vitest", "Read: src/a.ts"]
        );
        assert!(
            t.line().contains("shell: npx vitest 1 (1.2s)"),
            "{}",
            t.line()
        );
    }
}
