//! Everything the engine has to say is a typed event. The terminal printer
//! is one consumer; a JSON or web consumer is another file, never a change
//! to the engine.

use std::io::Write;
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
}

/// Prints events to stderr, one line at a time under a lock so concurrent
/// tasks never interleave mid-line. With `prefix` every line carries the
/// task id, which is what makes `--jobs 2` readable.
pub struct Reporter {
    prefix: bool,
}

static OUT: Mutex<()> = Mutex::new(());

impl Reporter {
    pub fn new(prefix: bool) -> Reporter {
        Reporter { prefix }
    }

    pub fn emit(&self, task_id: i64, ev: Event) {
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
    }
}
