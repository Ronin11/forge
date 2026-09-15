//! Everything the engine has to say is a typed event. The terminal printer
//! is one consumer; a JSON or web consumer is another file, never a change
//! to the engine.

use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const EVENT_LOG_SIZE_LIMIT: u64 = 50 * 1024 * 1024;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event<'a> {
    TaskStarted {
        worktree: &'a str,
        branch: &'a str,
        base_branch: &'a str,
        base_sha: &'a str,
        model: &'a str,
        max_turns: i64,
        max_attempts: i64,
        timeout_secs: i64,
        sandboxed: bool,
    },
    /// A task entered the queue: a client re-reads its listing.
    TaskQueued {
        workflow: &'a str,
        retry_of: Option<i64>,
    },
    AttemptStarted {
        n: i64,
        of: i64,
    },
    ToolCall {
        name: &'a str,
    },
    AgentDone {
        exit: Option<i32>,
        turns: i64,
        tools: i64,
        ms: u128,
        #[serde(rename = "cost_usd")]
        cost: Option<f64>,
        timed_out: bool,
    },
    GitCounted {
        commits: i64,
        files: i64,
        dirty: bool,
    },
    Check {
        level: &'a str,
        name: &'a str,
        ok: bool,
        ms: u128,
        tail: &'a str,
    },
    AttemptDone {
        state: &'a str,
        reason: &'a str,
    },
    Pushed {
        remote: &'a str,
        branch: &'a str,
    },
    PushFailed {
        error: &'a str,
    },
    PushSkipped,
    TaskDone {
        state: &'a str,
        attempts: usize,
        #[serde(rename = "cost_usd")]
        cost: f64,
        reason: &'a str,
        branch: &'a str,
        pushed: bool,
        compare: Option<&'a str>,
        #[serde(skip)]
        remove_cmd: &'a str,
    },
    Note {
        text: &'a str,
    },
    /// The operator decided this task should not be done: terminal, and
    /// never a defect in the work.
    TaskWithdrawn {
        reason: &'a str,
    },
    Op {
        name: &'a str,
        kernel: bool,
        ok: bool,
        ms: u128,
        detail: &'a str,
    },
}

impl Event<'_> {
    /// The first line the terminal would show: what a client needs when it
    /// wants words instead of a renderer, and what `render` builds on.
    pub fn summary(&self) -> String {
        match self {
            Event::TaskStarted {
                branch,
                base_branch,
                base_sha,
                ..
            } => format!(
                "branch {branch} from {base_branch} @ {}",
                &base_sha[..base_sha.len().min(8)]
            ),
            Event::TaskQueued { workflow, retry_of } => match retry_of {
                Some(old) => format!("queued {workflow}, a retry of task {old}"),
                None => format!("queued {workflow}"),
            },
            Event::AttemptStarted { n, of } => format!("attempt {n} of {of}"),
            Event::ToolCall { name } => format!("tool {name}"),
            Event::AgentDone {
                exit,
                turns,
                tools,
                ms,
                cost,
                timed_out,
            } => format!(
                "agent {} · {turns} turns · {tools} tool calls · {:.1}s · {}",
                if *timed_out {
                    "timed out".to_string()
                } else {
                    format!("exit {}", exit.map_or("signal".into(), |c| c.to_string()))
                },
                *ms as f64 / 1000.0,
                money(*cost)
            ),
            Event::GitCounted {
                commits,
                files,
                dirty,
            } => format!(
                "git {commits} commit(s), {files} file(s){}",
                if *dirty { ", dirty" } else { "" }
            ),
            Event::Check {
                level, name, ok, ..
            } => format!("{} {level} {name}", if *ok { "✓" } else { "✗" }),
            Event::AttemptDone { state, reason } => format!(
                "attempt {state}{}",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                }
            ),
            Event::Pushed { branch, .. } => format!("pushed {branch}"),
            Event::PushFailed { error } => format!("push failed: {error}"),
            Event::PushSkipped => "push skipped".into(),
            Event::TaskDone { state, reason, .. } => format!(
                "{state}{}",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                }
            ),
            Event::Note { text } => text.to_string(),
            Event::TaskWithdrawn { reason } => format!("withdrawn: {reason}"),
            Event::Op {
                name, ok, detail, ..
            } => format!(
                "op {} {name} {}",
                if *ok { "✓" } else { "✗" },
                detail.lines().next().unwrap_or("")
            ),
        }
    }
}

/// Prints events to stderr, one line at a time under a lock so concurrent
/// tasks never interleave mid-line. With `prefix` every line carries the
/// task id, which is what makes `--jobs 2` readable.
/// Where the event log goes when a home is known; every emitted event is
/// appended as one JSON line, the client's subscription.
pub struct Reporter {
    log: Option<PathBuf>,
    prefix: bool,
}

static OUT: Mutex<()> = Mutex::new(());

impl Reporter {
    pub fn new(prefix: bool, log: Option<PathBuf>) -> Reporter {
        Reporter { log, prefix }
    }

    fn append_log(&self, task_id: i64, ev: &Event) {
        self.append_log_with_limit(task_id, ev, EVENT_LOG_SIZE_LIMIT);
    }

    fn append_log_with_limit(&self, task_id: i64, ev: &Event, size_limit: u64) {
        let Some(path) = &self.log else {
            return;
        };
        let mut v = to_json(ev);
        v["ts"] = serde_json::json!(crate::unix_now());
        v["task"] = serde_json::json!(task_id);
        // Bounded: past size_limit the log rolls to .1 and .1 rolls to .2; a client resnapshots.
        if std::fs::metadata(path)
            .map(|m| m.len() > size_limit)
            .unwrap_or(false)
        {
            let path_1 = path.with_extension("jsonl.1");
            let path_2 = path.with_extension("jsonl.2");

            if path_1.exists() {
                let _ = std::fs::rename(&path_1, &path_2);
            }
            let _ = std::fs::rename(path, &path_1);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{v}");
        }
    }

    pub fn emit(&self, task_id: i64, ev: Event) {
        self.append_log(task_id, &ev);
        let lines = render(ev);
        let _guard = OUT.lock().unwrap_or_else(|p| p.into_inner());
        let mut err = std::io::stderr().lock();
        for l in lines {
            let _ = if self.prefix {
                writeln!(err, "[{task_id}] {l}")
            } else {
                writeln!(err, "{l}")
            };
        }
    }
}

/// The event as a client sees it: a `type`, its fields, and `text`, the
/// first line the terminal would show, so a client needs no renderer.
pub fn to_json(ev: &Event) -> serde_json::Value {
    let mut v = serde_json::to_value(ev).expect("Event always serializes");
    v["text"] = serde_json::json!(ev.summary());
    v
}

fn money(c: Option<f64>) -> String {
    c.map_or("cost n/a".to_string(), |c| format!("${c:.4}"))
}

fn render(ev: Event) -> Vec<String> {
    let summary = ev.summary();
    match ev {
        Event::TaskStarted {
            worktree,
            branch,
            base_branch,
            base_sha,
            model,
            max_turns,
            max_attempts,
            timeout_secs,
            sandboxed,
        } => vec![
            format!("worktree {worktree}"),
            format!(
                "branch   {branch} (from {base_branch} @ {})",
                &base_sha[..base_sha.len().min(8)]
            ),
            format!(
                "agent    {model}, max {max_turns} turns, max {max_attempts} attempts, {timeout_secs}s timeout{}",
                if sandboxed {
                    ", sandboxed"
                } else {
                    ", UNSANDBOXED"
                }
            ),
        ],
        Event::TaskQueued { .. } => vec![],
        Event::AttemptStarted { .. } => vec![format!("--- {summary}")],
        Event::ToolCall { name } => vec![format!("  ▸ {name}")],
        Event::AgentDone {
            exit,
            turns,
            tools,
            ms,
            cost,
            timed_out,
        } => vec![format!(
            "agent    {} · {turns} turns · {tools} tool calls · {:.1}s · {}",
            if timed_out {
                "TIMED OUT".to_string()
            } else {
                format!("exit {}", exit.map_or("signal".into(), |c| c.to_string()))
            },
            ms as f64 / 1000.0,
            money(cost)
        )],
        Event::GitCounted {
            commits,
            files,
            dirty,
        } => vec![format!(
            "git      {commits} commit(s), {files} file(s) changed{}",
            if dirty {
                ", UNCOMMITTED changes left behind"
            } else {
                ""
            }
        )],
        Event::Check { ok, ms, tail, .. } => {
            let mut v = vec![format!("  {summary} ({:.1}s)", ms as f64 / 1000.0)];
            if !ok {
                v.extend(tail.lines().map(|l| format!("      {l}")));
            }
            v
        }
        Event::AttemptDone { state, reason } => {
            vec![format!(
                "attempt  {state}{}",
                if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                }
            )]
        }
        Event::Pushed { remote, branch } => vec![format!("pushed   {remote}/{branch}")],
        Event::PushFailed { error } => vec![format!("push     FAILED: {error}")],
        Event::PushSkipped => vec!["push     skipped (no remote configured)".to_string()],
        Event::TaskDone {
            state,
            attempts,
            cost,
            reason,
            branch,
            pushed,
            compare,
            remove_cmd,
        } => {
            let mut v = vec![
                String::new(),
                format!(
                    "{}  ({attempts} attempt(s), ${cost:.4}){}",
                    state.to_uppercase(),
                    if reason.is_empty() {
                        String::new()
                    } else {
                        format!(": {reason}")
                    }
                ),
                format!(
                    "  branch   {branch}{}",
                    if pushed { " (pushed)" } else { "" }
                ),
            ];
            if let Some(u) = compare {
                v.push(format!("  compare  {u}"));
            }
            v.push(format!("  remove   {remove_cmd}"));
            v
        }
        Event::Note { .. } => vec![summary],
        Event::TaskWithdrawn { .. } => vec![String::new(), summary],
        Event::Op {
            name,
            kernel,
            ok,
            ms,
            detail,
        } => vec![format!(
            "op       {} {}{} ({:.1}s){}",
            if ok { "✓" } else { "✗" },
            name,
            if kernel { "" } else { " [user]" },
            ms as f64 / 1000.0,
            if detail.is_empty() || (ok && kernel) {
                String::new()
            } else {
                format!(": {}", detail.lines().next().unwrap_or(""))
            }
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    /// Golden values for what the hand-written `to_json` used to emit,
    /// one per variant, so the derived `Serialize` impl stays byte-for-byte
    /// compatible with it (including the `text` summary line and the
    /// `cost_usd`/`remove_cmd` deviations from the field names).
    #[test]
    fn to_json_matches_the_hand_written_shape_for_every_variant() {
        assert_eq!(
            to_json(&Event::TaskStarted {
                worktree: "wt",
                branch: "feat",
                base_branch: "main",
                base_sha: "abcdef1234567890",
                model: "m",
                max_turns: 10,
                max_attempts: 3,
                timeout_secs: 600,
                sandboxed: true,
            }),
            json!({
                "type": "task_started", "worktree": "wt", "branch": "feat",
                "base_branch": "main", "base_sha": "abcdef1234567890", "model": "m",
                "max_turns": 10, "max_attempts": 3, "timeout_secs": 600, "sandboxed": true,
                "text": "branch feat from main @ abcdef12",
            })
        );

        assert_eq!(
            to_json(&Event::TaskQueued {
                workflow: "wf",
                retry_of: Some(7),
            }),
            json!({
                "type": "task_queued", "workflow": "wf", "retry_of": 7,
                "text": "queued wf, a retry of task 7",
            })
        );

        assert_eq!(
            to_json(&Event::AttemptStarted { n: 2, of: 5 }),
            json!({"type": "attempt_started", "n": 2, "of": 5, "text": "attempt 2 of 5"})
        );

        assert_eq!(
            to_json(&Event::ToolCall { name: "grep" }),
            json!({"type": "tool_call", "name": "grep", "text": "tool grep"})
        );

        assert_eq!(
            to_json(&Event::AgentDone {
                exit: Some(0),
                turns: 4,
                tools: 9,
                ms: 1500,
                cost: Some(0.1234),
                timed_out: false,
            }),
            json!({
                "type": "agent_done", "exit": 0, "turns": 4, "tools": 9, "ms": 1500,
                "cost_usd": 0.1234, "timed_out": false,
                "text": "agent exit 0 · 4 turns · 9 tool calls · 1.5s · $0.1234",
            })
        );

        assert_eq!(
            to_json(&Event::GitCounted {
                commits: 3,
                files: 7,
                dirty: true,
            }),
            json!({
                "type": "git_counted", "commits": 3, "files": 7, "dirty": true,
                "text": "git 3 commit(s), 7 file(s), dirty",
            })
        );

        assert_eq!(
            to_json(&Event::Check {
                level: "l1",
                name: "tests",
                ok: false,
                ms: 2500,
                tail: "line1\nline2",
            }),
            json!({
                "type": "check", "level": "l1", "name": "tests", "ok": false, "ms": 2500,
                "tail": "line1\nline2", "text": "✗ l1 tests",
            })
        );

        assert_eq!(
            to_json(&Event::AttemptDone {
                state: "verified",
                reason: "",
            }),
            json!({
                "type": "attempt_done", "state": "verified", "reason": "",
                "text": "attempt verified",
            })
        );

        assert_eq!(
            to_json(&Event::Pushed {
                remote: "origin",
                branch: "main",
            }),
            json!({
                "type": "pushed", "remote": "origin", "branch": "main",
                "text": "pushed main",
            })
        );

        assert_eq!(
            to_json(&Event::PushFailed { error: "denied" }),
            json!({
                "type": "push_failed", "error": "denied",
                "text": "push failed: denied",
            })
        );

        assert_eq!(
            to_json(&Event::PushSkipped),
            json!({"type": "push_skipped", "text": "push skipped"})
        );

        assert_eq!(
            to_json(&Event::TaskDone {
                state: "success",
                attempts: 2,
                cost: 1.5,
                reason: "",
                branch: "feat",
                pushed: true,
                compare: Some("http://x"),
                remove_cmd: "rm -rf x",
            }),
            json!({
                "type": "task_done", "state": "success", "attempts": 2, "cost_usd": 1.5,
                "reason": "", "branch": "feat", "pushed": true, "compare": "http://x",
                "text": "success",
            })
        );

        assert_eq!(
            to_json(&Event::Note { text: "hello" }),
            json!({"type": "note", "text": "hello"})
        );

        assert_eq!(
            to_json(&Event::Op {
                name: "build",
                kernel: true,
                ok: true,
                ms: 800,
                detail: "ok\nmore",
            }),
            json!({
                "type": "op", "name": "build", "kernel": true, "ok": true, "ms": 800,
                "detail": "ok\nmore", "text": "op ✓ build ok",
            })
        );
    }

    #[test]
    fn test_event_log_rotation_keeps_two_generations() {
        let temp_dir = std::env::temp_dir().join("forge_test_events_rotation");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let log_path = temp_dir.join("events.jsonl");
        let reporter = Reporter::new(false, Some(log_path.clone()));

        const TEST_SIZE_LIMIT: u64 = 100;
        let event = Event::Note {
            text: "test event content",
        };

        for i in 0..15 {
            reporter.append_log_with_limit(i, &event, TEST_SIZE_LIMIT);
        }

        assert!(
            log_path.exists(),
            "events.jsonl should exist after rotation"
        );
        assert!(
            log_path.with_extension("jsonl.1").exists(),
            "events.jsonl.1 should exist after rotation"
        );
        assert!(
            log_path.with_extension("jsonl.2").exists(),
            "events.jsonl.2 should exist after rotation"
        );

        let size_0 = fs::metadata(&log_path).unwrap().len();
        let size_1 = fs::metadata(log_path.with_extension("jsonl.1"))
            .unwrap()
            .len();
        let size_2 = fs::metadata(log_path.with_extension("jsonl.2"))
            .unwrap()
            .len();

        assert!(size_0 > 0, "events.jsonl should have content");
        assert!(size_1 > 0, "events.jsonl.1 should have content");
        assert!(size_2 > 0, "events.jsonl.2 should have content");

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
