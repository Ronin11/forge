//! Everything the engine has to say is a typed event. The terminal printer
//! is one consumer; a JSON or web consumer is another file, never a change
//! to the engine.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

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
        cost: f64,
        reason: &'a str,
        branch: &'a str,
        pushed: bool,
        compare: Option<&'a str>,
        remove_cmd: &'a str,
    },
    Note {
        text: &'a str,
    },
    Op {
        name: &'a str,
        kernel: bool,
        ok: bool,
        ms: u128,
        detail: &'a str,
    },
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
        let Some(path) = &self.log else {
            return;
        };
        let mut v = to_json(ev);
        v["ts"] = serde_json::json!(crate::unix_now());
        v["task"] = serde_json::json!(task_id);
        // Bounded: past 50 MB the log rolls to .1; a client resnapshots.
        if std::fs::metadata(path)
            .map(|m| m.len() > 50 * 1024 * 1024)
            .unwrap_or(false)
        {
            let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
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
    use serde_json::json;
    let text = match ev {
        Event::TaskStarted {
            branch,
            base_branch,
            base_sha,
            ..
        } => format!(
            "branch {branch} from {base_branch} @ {}",
            &base_sha[..base_sha.len().min(8)]
        ),
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
        Event::Op {
            name, ok, detail, ..
        } => format!(
            "op {} {name} {}",
            if *ok { "✓" } else { "✗" },
            detail.lines().next().unwrap_or("")
        ),
    };
    let mut v = match ev {
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
        } => {
            json!({"type": "task_started", "worktree": worktree, "branch": branch, "base_branch": base_branch, "base_sha": base_sha, "model": model, "max_turns": max_turns, "max_attempts": max_attempts, "timeout_secs": timeout_secs, "sandboxed": sandboxed})
        }
        Event::AttemptStarted { n, of } => json!({"type": "attempt_started", "n": n, "of": of}),
        Event::ToolCall { name } => json!({"type": "tool_call", "name": name}),
        Event::AgentDone {
            exit,
            turns,
            tools,
            ms,
            cost,
            timed_out,
        } => {
            json!({"type": "agent_done", "exit": exit, "turns": turns, "tools": tools, "ms": *ms as u64, "cost_usd": cost, "timed_out": timed_out})
        }
        Event::GitCounted {
            commits,
            files,
            dirty,
        } => json!({"type": "git_counted", "commits": commits, "files": files, "dirty": dirty}),
        Event::Check {
            level,
            name,
            ok,
            ms,
            tail,
        } => {
            json!({"type": "check", "level": level, "name": name, "ok": ok, "ms": *ms as u64, "tail": tail})
        }
        Event::AttemptDone { state, reason } => {
            json!({"type": "attempt_done", "state": state, "reason": reason})
        }
        Event::Pushed { remote, branch } => {
            json!({"type": "pushed", "remote": remote, "branch": branch})
        }
        Event::PushFailed { error } => json!({"type": "push_failed", "error": error}),
        Event::PushSkipped => json!({"type": "push_skipped"}),
        Event::TaskDone {
            state,
            attempts,
            cost,
            reason,
            branch,
            pushed,
            compare,
            remove_cmd: _,
        } => {
            json!({"type": "task_done", "state": state, "attempts": attempts, "cost_usd": cost, "reason": reason, "branch": branch, "pushed": pushed, "compare": compare})
        }
        Event::Note { text } => json!({"type": "note", "text": text}),
        Event::Op {
            name,
            kernel,
            ok,
            ms,
            detail,
        } => {
            json!({"type": "op", "name": name, "kernel": kernel, "ok": ok, "ms": *ms as u64, "detail": detail})
        }
    };
    if v.get("text").is_none() {
        v["text"] = json!(text);
    }
    v
}

fn money(c: Option<f64>) -> String {
    c.map_or("cost n/a".to_string(), |c| format!("${c:.4}"))
}

fn render(ev: Event) -> Vec<String> {
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
        Event::AttemptStarted { n, of } => vec![format!("--- attempt {n} of {of}")],
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
        Event::Check {
            level,
            name,
            ok,
            ms,
            tail,
        } => {
            let mut v = vec![format!(
                "  {} {level} {name} ({:.1}s)",
                if ok { "✓" } else { "✗" },
                ms as f64 / 1000.0
            )];
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
        Event::Note { text } => vec![text.to_string()],
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
