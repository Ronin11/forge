//! Everything the engine has to say is a typed event. The terminal printer
//! is one consumer; a JSON or web consumer is another file, never a change
//! to the engine.

use serde::Serialize;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    /// The last of an initiative's tasks reached a terminal state: its
    /// own record is closed (see docs/PROJECTS.md, "One notification and
    /// one report").
    InitiativeSettled {
        id: i64,
        state: &'a str,
        #[serde(rename = "cost_usd")]
        cost: f64,
    },
    /// A deploy began (see docs/DEPLOY.md, "When a deploy runs").
    DeployStarted {
        project: &'a str,
        target: &'a str,
        sha: &'a str,
    },
    /// A deploy reached a verdict: `ok` is the check's result, and
    /// `rolled_back_to` is the previous passing commit it fell back to
    /// when the check failed (`None` if it passed, or if there was
    /// nothing to roll back to).
    DeployFinished {
        project: &'a str,
        target: &'a str,
        sha: &'a str,
        ok: bool,
        rolled_back_to: Option<&'a str>,
    },
    /// `forge intake accept` created a project for the first time (see
    /// docs/INTAKE.md): a plugin's cue to send the person who asked for
    /// it their customer portal link (see docs/PORTAL.md).
    ProjectCreated {
        project: &'a str,
        person: &'a str,
    },
    /// A job began running its steps (see docs/JOBS.md, "The executor"):
    /// both `forge job start --now` and the worker's claimed run
    /// (`job::drive`) emit this once the archive and input are ready.
    JobStarted {
        project: &'a str,
        workflow: &'a str,
        job_id: i64,
        dry_run: bool,
    },
    /// A job reached a final state: `ok`, `failed`, `needs_human` or
    /// `dropped` (see docs/JOBS.md, "Vocabulary").
    JobFinished {
        project: &'a str,
        workflow: &'a str,
        job_id: i64,
        state: &'a str,
        cost_usd: f64,
    },
}

/// Every `type` an event carries in `events.jsonl`, the values a run
/// workflow's `[trigger] on = "event"` may name as its `type` (docs/JOBS.md,
/// "Triggers"). The test below keeps it equal to the enum's variants.
pub const EVENT_TYPES: &[&str] = &[
    "task_started",
    "task_queued",
    "attempt_started",
    "tool_call",
    "agent_done",
    "git_counted",
    "check",
    "attempt_done",
    "pushed",
    "push_failed",
    "push_skipped",
    "task_done",
    "note",
    "task_withdrawn",
    "op",
    "initiative_settled",
    "deploy_started",
    "deploy_finished",
    "project_created",
    "job_started",
    "job_finished",
];

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
            Event::InitiativeSettled { id, state, cost } => {
                format!("initiative {id} settled {state} ({})", money(Some(*cost)))
            }
            Event::DeployStarted {
                project,
                target,
                sha,
            } => format!(
                "deploying {project}/{target} @ {}",
                &sha[..sha.len().min(8)]
            ),
            Event::DeployFinished {
                project,
                target,
                sha,
                ok,
                rolled_back_to,
            } => match (*ok, rolled_back_to) {
                (true, _) => format!("{project}/{target} is live at {}", &sha[..sha.len().min(8)]),
                (false, Some(to)) => format!(
                    "deploy of {project}/{target} @ {} failed its check and was rolled back to {}",
                    &sha[..sha.len().min(8)],
                    &to[..to.len().min(8)]
                ),
                (false, None) => format!(
                    "deploy of {project}/{target} @ {} failed its check; nothing to roll back to",
                    &sha[..sha.len().min(8)]
                ),
            },
            Event::ProjectCreated { project, person } => {
                format!("created project {project} for {person}")
            }
            Event::JobStarted {
                project,
                workflow,
                job_id,
                dry_run,
            } => format!(
                "running {project}/{workflow} (job {job_id}){}",
                if *dry_run { ", dry run" } else { "" }
            ),
            Event::JobFinished {
                project,
                workflow,
                job_id,
                state,
                cost_usd,
            } => format!(
                "job {job_id} ({project}/{workflow}) {state} ({})",
                money(Some(*cost_usd))
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
    quiet: bool,
    /// Tasks a write failure has already been surfaced for, so a directory
    /// that stays unwritable for an entire attempt costs one `Note`, not
    /// one per event (see `note_dropped`).
    noted_drops: Mutex<HashSet<i64>>,
}

static OUT: Mutex<()> = Mutex::new(());

/// The file `dropped_log_task_count` reads: one task id per line, appended
/// once the first time that task's event log write fails. Sits beside the
/// log itself (`events.jsonl` -> `events.dropped`) so it shares the log's
/// directory and survives across processes without a database dependency.
fn dropped_marker_path(log_path: &Path) -> PathBuf {
    log_path.with_extension("dropped")
}

/// How many distinct tasks have lost at least one event-log line, for
/// `forge doctor`'s `logs` check. Reads the marker file `note_dropped`
/// maintains; missing or unreadable means zero, never an error.
pub fn dropped_log_task_count(log_path: &Path) -> usize {
    let Ok(content) = std::fs::read_to_string(dropped_marker_path(log_path)) else {
        return 0;
    };
    content
        .lines()
        .filter(|l| !l.is_empty())
        .collect::<HashSet<_>>()
        .len()
}

impl Reporter {
    pub fn new(prefix: bool, log: Option<PathBuf>) -> Reporter {
        Reporter {
            log,
            prefix,
            quiet: false,
            noted_drops: Mutex::new(HashSet::new()),
        }
    }

    /// Prints and logs nothing: for a run whose output is a report of its
    /// own (`forge job test`).
    pub fn quiet() -> Reporter {
        Reporter {
            log: None,
            prefix: false,
            quiet: true,
            noted_drops: Mutex::new(HashSet::new()),
        }
    }

    fn append_log(&self, task_id: i64, ev: &Event) {
        self.append_log_with_limit(task_id, ev, EVENT_LOG_SIZE_LIMIT);
    }

    /// The first time `task_id`'s event log write fails, records it in the
    /// durable marker file and prints one `Note` directly to stderr (never
    /// through `append_log`, which is exactly what just failed). Every
    /// later failure for the same task is silent: the operator already
    /// knows. The dedup holds across processes because it consults the
    /// marker file — a `forge work` restart mid-task (the orphan-requeue
    /// path in worker.rs) starts a fresh `Reporter` with an empty
    /// in-memory set, but the marker file still names the task.
    fn note_dropped(&self, task_id: i64, path: &Path, err: &std::io::Error) {
        let mut noted = self.noted_drops.lock().unwrap_or_else(|p| p.into_inner());
        if !noted.insert(task_id) {
            return;
        }
        drop(noted);
        if let Ok(content) = std::fs::read_to_string(dropped_marker_path(path)) {
            let id = task_id.to_string();
            if content.lines().any(|l| l.trim() == id) {
                return;
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dropped_marker_path(path))
        {
            let _ = writeln!(f, "{task_id}");
        }
        let text = format!("event log write failed at {}: {err}", path.display());
        let lines = render(Event::Note { text: &text });
        let _guard = OUT.lock().unwrap_or_else(|p| p.into_inner());
        let mut errw = std::io::stderr().lock();
        for l in lines {
            let _ = if self.prefix {
                writeln!(errw, "[{task_id}] {l}")
            } else {
                writeln!(errw, "{l}")
            };
        }
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
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{v}"));
        if let Err(e) = result {
            self.note_dropped(task_id, path, &e);
        }
    }

    pub fn emit(&self, task_id: i64, ev: Event) {
        if self.quiet {
            return;
        }
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
        Event::InitiativeSettled { .. } => vec![String::new(), summary],
        Event::DeployStarted { .. } => vec![summary],
        Event::DeployFinished { .. } => vec![String::new(), summary],
        Event::ProjectCreated { .. } => vec![summary],
        Event::JobStarted { .. } => vec![summary],
        Event::JobFinished { .. } => vec![String::new(), summary],
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

        assert_eq!(
            to_json(&Event::InitiativeSettled {
                id: 5,
                state: "done",
                cost: 2.5,
            }),
            json!({
                "type": "initiative_settled", "id": 5, "state": "done", "cost_usd": 2.5,
                "text": "initiative 5 settled done ($2.5000)",
            })
        );

        assert_eq!(
            to_json(&Event::DeployStarted {
                project: "equitizr",
                target: "prod",
                sha: "abcdef1234567890",
            }),
            json!({
                "type": "deploy_started", "project": "equitizr", "target": "prod",
                "sha": "abcdef1234567890", "text": "deploying equitizr/prod @ abcdef12",
            })
        );

        assert_eq!(
            to_json(&Event::DeployFinished {
                project: "equitizr",
                target: "prod",
                sha: "abcdef1234567890",
                ok: false,
                rolled_back_to: Some("1234567890abcdef"),
            }),
            json!({
                "type": "deploy_finished", "project": "equitizr", "target": "prod",
                "sha": "abcdef1234567890", "ok": false, "rolled_back_to": "1234567890abcdef",
                "text": "deploy of equitizr/prod @ abcdef12 failed its check and was rolled back to 12345678",
            })
        );

        assert_eq!(
            to_json(&Event::ProjectCreated {
                project: "nate",
                person: "nate",
            }),
            json!({
                "type": "project_created", "project": "nate", "person": "nate",
                "text": "created project nate for nate",
            })
        );

        assert_eq!(
            to_json(&Event::JobStarted {
                project: "equitizr",
                workflow: "quote-by-text",
                job_id: 9,
                dry_run: false,
            }),
            json!({
                "type": "job_started", "project": "equitizr", "workflow": "quote-by-text",
                "job_id": 9, "dry_run": false,
                "text": "running equitizr/quote-by-text (job 9)",
            })
        );

        assert_eq!(
            to_json(&Event::JobFinished {
                project: "equitizr",
                workflow: "quote-by-text",
                job_id: 9,
                state: "ok",
                cost_usd: 0.05,
            }),
            json!({
                "type": "job_finished", "project": "equitizr", "workflow": "quote-by-text",
                "job_id": 9, "state": "ok", "cost_usd": 0.05,
                "text": "job 9 (equitizr/quote-by-text) ok ($0.0500)",
            })
        );
    }

    #[test]
    fn event_types_names_every_variant_of_the_enum() {
        let src = include_str!("report.rs");
        let body = src
            .split("pub enum Event<'a> {\n")
            .nth(1)
            .and_then(|r| r.split("\n}\n").next())
            .unwrap();
        let mut variants: Vec<String> = body
            .lines()
            .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
            .filter_map(|l| {
                let name: String = l
                    .trim()
                    .chars()
                    .take_while(|c| c.is_alphanumeric())
                    .collect();
                name.chars().next().filter(|c| c.is_uppercase())?;
                Some(name)
            })
            .map(|n| {
                let mut out = String::new();
                for (i, c) in n.chars().enumerate() {
                    if c.is_uppercase() && i > 0 {
                        out.push('_');
                    }
                    out.push(c.to_ascii_lowercase());
                }
                out
            })
            .collect();
        variants.sort();
        let mut listed: Vec<String> = EVENT_TYPES.iter().map(|s| s.to_string()).collect();
        listed.sort();
        assert_eq!(listed, variants);
        // And the name the list carries is the one an event serializes under.
        assert_eq!(
            to_json(&Event::TaskDone {
                state: "succeeded",
                attempts: 1,
                cost: 0.0,
                reason: "",
                branch: "b",
                pushed: false,
                compare: None,
                remove_cmd: "",
            })["type"],
            "task_done"
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

    /// A log path under a directory this process cannot write to: many
    /// failed events for the same task cost exactly one `Note` (checked via
    /// the marker file `note_dropped` appends to, since a unit test has no
    /// clean way to assert on stderr), and `dropped_log_task_count` — what
    /// `forge doctor`'s `logs` check reports — counts that one task once.
    #[test]
    fn a_write_failure_notes_once_per_task_and_counts_for_doctor() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = std::env::temp_dir().join("forge_test_events_dropped");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        let locked = temp_dir.join("locked");
        fs::create_dir_all(&locked).unwrap();
        let log_path = locked.join("events.jsonl");
        // The marker file must already exist: once the directory loses its
        // write bit, appending to an existing file still works (only
        // traversal is needed), but creating `events.jsonl` for the first
        // time does not — which is exactly the failure under test.
        fs::write(log_path.with_extension("dropped"), b"").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();

        let reporter = Reporter::new(false, Some(log_path.clone()));
        let event = Event::Note {
            text: "does not matter",
        };
        for _ in 0..30 {
            reporter.append_log_with_limit(1, &event, EVENT_LOG_SIZE_LIMIT);
        }
        // A second task's first failure still gets its own note.
        reporter.append_log_with_limit(2, &event, EVENT_LOG_SIZE_LIMIT);

        let marker = std::fs::read_to_string(log_path.with_extension("dropped")).unwrap();
        assert_eq!(
            marker.lines().collect::<Vec<_>>(),
            vec!["1", "2"],
            "one marker line per task, not per event"
        );
        assert_eq!(dropped_log_task_count(&log_path), 2);

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn a_second_reporter_does_not_renote_a_task_the_marker_already_names() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = std::env::temp_dir().join("forge_test_events_dropped_restart");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        let locked = temp_dir.join("locked");
        fs::create_dir_all(&locked).unwrap();
        let log_path = locked.join("events.jsonl");
        fs::write(log_path.with_extension("dropped"), b"").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();

        let event = Event::Note {
            text: "does not matter",
        };

        // Simulates a `forge work` restart mid-task: the first Reporter's
        // process exits (e.g. the orphan-requeue path in worker.rs) and a
        // second Reporter picks the same task back up with an empty
        // in-memory `noted_drops` set.
        let first = Reporter::new(false, Some(log_path.clone()));
        for _ in 0..10 {
            first.append_log_with_limit(42, &event, EVENT_LOG_SIZE_LIMIT);
        }

        let second = Reporter::new(false, Some(log_path.clone()));
        for _ in 0..10 {
            second.append_log_with_limit(42, &event, EVENT_LOG_SIZE_LIMIT);
        }

        let marker = std::fs::read_to_string(log_path.with_extension("dropped")).unwrap();
        assert_eq!(
            marker.lines().collect::<Vec<_>>(),
            vec!["42"],
            "the marker file already named the task, so the second process must not append again"
        );
        assert_eq!(dropped_log_task_count(&log_path), 1);

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        let _ = fs::remove_dir_all(&temp_dir);
    }
}
