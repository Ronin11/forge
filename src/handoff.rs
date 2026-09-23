//! The fresh arm of the `continuation` factor (docs/CONTEXT.md): a capped
//! attempt continues in a new session whose handoff Forge writes from
//! facts (git, the journal, the previous session's log), never from a
//! model summarising.

use crate::ctx::Forge;
use crate::store::Task;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// A continuation is due once the last request's context passes this many
/// tokens, when the log says so.
pub const CONTEXT_THRESHOLD_TOKENS: i64 = 120_000;

/// The context the session's last request carried: input plus cached
/// tokens from the log's last assistant usage.
pub fn last_context_tokens(log: &str) -> Option<i64> {
    log.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "assistant")
        .filter_map(|v| {
            let u = &v["message"]["usage"];
            u.is_object().then(|| {
                [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .map(|k| u[k].as_i64().unwrap_or(0))
                .sum()
            })
        })
        .next_back()
}

/// The distinct file paths the session read, in first-read order.
pub fn files_read(log: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for v in log
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        if v["type"] != "assistant" {
            continue;
        }
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            if b["type"] == "tool_use"
                && b["name"] == "Read"
                && let Some(p) = b["input"]["file_path"].as_str()
                && seen.insert(p.to_string())
            {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// The session's last structured output, else its last result text.
pub fn last_output(log: &str) -> Option<String> {
    log.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["type"] == "result")
        .filter_map(|v| match &v["structured_output"] {
            Value::Null => v["result"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string),
            s => Some(s.to_string()),
        })
        .next_back()
}

/// The handoff section, built entirely from facts.
pub async fn build(f: &Forge, t: &Task, dir: &Path, start_sha: &str, prev_log: &Path) -> String {
    let log = std::fs::read_to_string(prev_log).unwrap_or_default();
    let mut s = String::from(
        "\n\nHandoff: you are continuing this task in a fresh session. The previous session's \
         work is on the branch; Forge wrote what follows from git, the journal and that session's log.\n",
    );
    let commits = crate::git::log_oneline(dir, start_sha)
        .await
        .unwrap_or_default();
    s.push_str("\nCommits on the branch since the attempt started (git log --oneline):\n");
    s.push_str(if commits.is_empty() {
        "  (none)"
    } else {
        &commits
    });
    let stat = crate::git::diff_stat_tree(dir, start_sha)
        .await
        .unwrap_or_default();
    s.push_str("\n\nChanges so far (git diff --stat):\n");
    s.push_str(if stat.is_empty() { "  (none)" } else { &stat });
    let found: Vec<String> = crate::journal::journal_for(f, t)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.trim_start().starts_with("found:"))
        .map(|l| l.trim().to_string())
        .collect();
    s.push_str("\n\nWhat the checks found so far:\n");
    if found.is_empty() {
        s.push_str("  (nothing)");
    }
    for l in &found {
        s.push_str(&format!("  {l}\n"));
    }
    let read = files_read(&log);
    s.push_str("\n\nFiles the previous session read:\n");
    if read.is_empty() {
        s.push_str("  (none)");
    }
    for p in &read {
        s.push_str(&format!("  {p}\n"));
    }
    if let Some(last) = last_output(&log) {
        s.push_str(&format!(
            "\n\nThe previous session's last output:\n{last}\n"
        ));
    }
    s.push_str(&format!("\nThe task:\n{}\n", t.task));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = concat!(
        r#"{"type":"assistant","message":{"usage":{"input_tokens":5,"cache_read_input_tokens":100000,"cache_creation_input_tokens":20},"content":[{"type":"tool_use","id":"a","name":"Read","input":{"file_path":"src/a.rs"}}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"usage":{"input_tokens":7,"cache_read_input_tokens":130000},"content":[{"type":"tool_use","id":"b","name":"Read","input":{"file_path":"src/a.rs"}},{"type":"tool_use","id":"c","name":"Read","input":{"file_path":"src/b.rs"}}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"error_max_turns","result":"Reached max turns"}"#,
        "\n"
    );

    #[test]
    fn facts_come_from_the_log() {
        assert_eq!(last_context_tokens(LOG), Some(130_007));
        assert_eq!(files_read(LOG), ["src/a.rs", "src/b.rs"]);
        assert_eq!(last_output(LOG).as_deref(), Some("Reached max turns"));
    }
}
