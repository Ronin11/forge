//! Verification: claims are verified, not believed. Three levels, each run
//! by Forge after the agent has exited, each a row in the verdict.
//!
//! - L0: is the result consistent with git? A structured result exists,
//!   the tree is clean, there is at least one commit, `forge.toml` is
//!   untouched, the reported `changes[]` match what git saw, and every
//!   claim carries evidence.
//! - L1: do the repository's declared checks pass, read from the trusted
//!   base commit and run by Forge in the sandbox? And for every check the
//!   agent reported as passed, did Forge's run pass too? A claimed pass
//!   Forge cannot reproduce is the canonical false claim. The rule is
//!   one-directional: the agent may be conservative, never optimistic.
//! - L2: do the task's own acceptance commands pass? These are the
//!   operator's definition of done, declared with `--check`.
//!
//! A level runs only if the one before it passed. `decide` maps the rows to
//! a terminal state and is a pure function with a table test.

use crate::agent::Outcome;
use crate::checks::{CheckResult, last_lines, run_one};
use crate::config::Config;
use crate::envelope::{self, Envelope};
use crate::report::{Event, Reporter};
use crate::sandbox::Sandbox;
use crate::store::AttemptState;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

pub struct Subject<'a> {
    pub task_id: i64,
    pub worktree: &'a Path,
    pub repo_git_dir: &'a Path,
    pub base_sha: &'a str,
    pub cfg: &'a Config,
    pub task_checks: &'a [String],
    pub sandbox: Option<&'a Sandbox>,
    pub report: &'a Reporter,
}

pub struct Verdict {
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub envelope: Option<Envelope>,
    pub checks: Vec<CheckResult>,
    pub state: AttemptState,
    pub reason: String,
}

fn l0(name: &str, ok: bool, detail: String) -> CheckResult {
    CheckResult {
        level: "L0".into(),
        name: name.into(),
        ok,
        tail: if ok { String::new() } else { detail },
        ..Default::default()
    }
}

pub async fn verify(s: Subject<'_>, agent: &Outcome) -> Result<Verdict> {
    let commits = crate::git::count_commits(s.worktree, s.base_sha).await?;
    let changed = crate::git::changed_paths(s.worktree, s.base_sha).await?;
    let dirty = crate::git::dirty_paths(s.worktree).await?;
    s.report.emit(
        s.task_id,
        Event::GitCounted {
            commits,
            files: changed.len() as i64,
            dirty: !dirty.is_empty(),
        },
    );
    let mut v = Verdict {
        commits,
        files_changed: changed.len() as i64,
        dirty: !dirty.is_empty(),
        envelope: None,
        checks: Vec::new(),
        state: AttemptState::Running,
        reason: String::new(),
    };

    let agent_reason = agent_failure(agent);
    let mut needs_input: Option<String> = None;
    if agent_reason.is_none() {
        // L0
        let parsed = envelope::parse(agent.structured.as_deref(), &agent.result_text);
        let env = match &parsed {
            Ok(Some(e)) => Some(e.clone()),
            _ => None,
        };
        v.checks.push(l0(
            "result-structured",
            env.is_some(),
            match &parsed {
                Ok(None) => "the agent produced no structured result".into(),
                Err(e) => format!("the structured result does not fit the contract: {e}"),
                Ok(Some(_)) => String::new(),
            },
        ));
        if let Some(q) = env.as_ref().and_then(|e| e.needs_input.as_ref()) {
            needs_input = Some(q.question.clone());
        }
        v.checks.push(l0(
            "clean-tree",
            dirty.is_empty(),
            format!("uncommitted: {}", dirty.join(", ")),
        ));
        let touched = changed
            .iter()
            .chain(dirty.iter())
            .any(|p| p == "forge.toml");
        v.checks.push(l0(
            "forge.toml-untouched",
            !touched,
            "the attempt modified forge.toml".into(),
        ));
        v.checks.push(l0(
            "has-commits",
            commits > 0,
            "no commits on the branch".into(),
        ));
        if let Some(e) = &env {
            let reported: BTreeSet<&str> = e.changes.iter().map(|c| c.path.as_str()).collect();
            let actual: BTreeSet<&str> = changed
                .iter()
                .chain(dirty.iter())
                .map(String::as_str)
                .collect();
            let unreported: Vec<&str> = actual.difference(&reported).copied().collect();
            let phantom: Vec<&str> = reported.difference(&actual).copied().collect();
            let mut detail = String::new();
            if !unreported.is_empty() {
                detail.push_str(&format!(
                    "changed in git but not reported: {}\n",
                    unreported.join(", ")
                ));
            }
            if !phantom.is_empty() {
                detail.push_str(&format!(
                    "reported but unchanged in git: {}",
                    phantom.join(", ")
                ));
            }
            v.checks.push(l0(
                "changes-match-git",
                unreported.is_empty() && phantom.is_empty(),
                detail.trim().to_string(),
            ));
            let bare: Vec<&str> = e
                .claims
                .iter()
                .filter(|c| c.evidence.trim().is_empty())
                .map(|c| c.claim.as_str())
                .collect();
            v.checks.push(l0(
                "claims-have-evidence",
                bare.is_empty(),
                format!("claims without evidence: {}", bare.join("; ")),
            ));
        }
        for c in &v.checks {
            s.report.emit(
                s.task_id,
                Event::Check {
                    level: &c.level,
                    name: &c.name,
                    ok: c.ok,
                    ms: c.ms,
                    tail: &c.tail,
                },
            );
        }
        let l0_ok = v.checks.iter().all(|c| c.ok) && needs_input.is_none();

        // L1
        if l0_ok {
            let timeout = Duration::from_secs(s.cfg.check_timeout_secs);
            // `setup` runs first and gates the rest: without dependencies
            // installed the other checks would fail for the wrong reason.
            let mut names: Vec<&String> = s.cfg.checks.keys().collect();
            names.sort_by_key(|n| (n.as_str() != "setup", n.as_str()));
            for name in names {
                let argv = &s.cfg.checks[name];
                let r = run_one(
                    "L1",
                    name,
                    argv,
                    s.worktree,
                    s.repo_git_dir,
                    s.sandbox,
                    timeout,
                )
                .await;
                s.report.emit(
                    s.task_id,
                    Event::Check {
                        level: &r.level,
                        name: &r.name,
                        ok: r.ok,
                        ms: r.ms,
                        tail: &last_lines(&r.tail, 20),
                    },
                );
                let gate_failed = name == "setup" && !r.ok;
                v.checks.push(r);
                if gate_failed {
                    break;
                }
            }
            if let Some(e) = &env {
                for claimed in e.checks_run.iter().filter(|c| c.passed) {
                    let Some(ours) = v
                        .checks
                        .iter()
                        .find(|c| c.level == "L1" && c.name == claimed.check)
                    else {
                        continue;
                    };
                    if !ours.ok {
                        let r = CheckResult {
                            level: "L1".into(),
                            name: format!("claim:{}", claimed.check),
                            ok: false,
                            tail: format!(
                                "you reported `{}` passed; when Forge ran it, it failed ({})",
                                claimed.check,
                                ours.exit
                                    .map_or("no exit code".into(), |e| format!("exit {e}"))
                            ),
                            ..Default::default()
                        };
                        s.report.emit(
                            s.task_id,
                            Event::Check {
                                level: &r.level,
                                name: &r.name,
                                ok: false,
                                ms: 0,
                                tail: &r.tail,
                            },
                        );
                        v.checks.push(r);
                    }
                }
            }
        }
        let l1_ok = v.checks.iter().filter(|c| c.level == "L1").all(|c| c.ok);

        // L2
        if l0_ok && l1_ok {
            let timeout = Duration::from_secs(s.cfg.check_timeout_secs);
            for (i, cmd) in s.task_checks.iter().enumerate() {
                let name = format!("task-check-{}", i + 1);
                let argv = vec!["bash".to_string(), "-c".to_string(), cmd.clone()];
                let mut r = run_one(
                    "L2",
                    &name,
                    &argv,
                    s.worktree,
                    s.repo_git_dir,
                    s.sandbox,
                    timeout,
                )
                .await;
                if !r.ok {
                    r.tail = format!("$ {cmd}\n{}", r.tail);
                }
                s.report.emit(
                    s.task_id,
                    Event::Check {
                        level: &r.level,
                        name: &r.name,
                        ok: r.ok,
                        ms: r.ms,
                        tail: &last_lines(&r.tail, 20),
                    },
                );
                v.checks.push(r);
            }
        }
        v.envelope = env;
    }

    let (state, reason) = decide(agent_reason.as_deref(), needs_input.as_deref(), &v.checks);
    v.state = state;
    v.reason = reason;
    Ok(v)
}

/// Why the agent run itself counts as failed, if it does.
pub fn agent_failure(a: &Outcome) -> Option<String> {
    if a.timed_out {
        Some("agent timed out".into())
    } else if a.exit_code != Some(0) {
        Some(format!(
            "agent exit {}",
            a.exit_code.map_or("signal".into(), |c| c.to_string())
        ))
    } else if !a.got_result {
        Some("agent produced no result".into())
    } else if a.is_error {
        Some("agent reported an error".into())
    } else {
        None
    }
}

/// The verdict table. Pure: the same rows always give the same answer.
pub fn decide(
    agent_failure: Option<&str>,
    needs_input: Option<&str>,
    checks: &[CheckResult],
) -> (AttemptState, String) {
    if let Some(why) = agent_failure {
        return (AttemptState::AgentFailed, why.to_string());
    }
    if let Some(q) = needs_input {
        return (AttemptState::NeedsInput, format!("needs input: {q}"));
    }
    for level in ["L0", "L1", "L2"] {
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| c.level == level && !c.ok)
            .map(|c| c.name.as_str())
            .collect();
        if !failed.is_empty() {
            return (
                AttemptState::ChecksFailed,
                format!("{level} failed: {}", failed.join(", ")),
            );
        }
    }
    let verified = checks.iter().any(|c| c.level == "L1" || c.level == "L2");
    if !verified {
        return (
            AttemptState::Unverified,
            "no L1 or L2 checks; nothing verified the work".into(),
        );
    }
    (AttemptState::Succeeded, String::new())
}

/// What the next attempt is told. Specific enough to act on, nothing more.
pub fn feedback(v: &Verdict, agent: &Outcome, max_turns: i64) -> String {
    match v.state {
        AttemptState::AgentFailed => format!(
            "The previous attempt ended without a result ({}; {} turns of {max_turns} allowed). \
             Continue from the current state of the branch, be economical with turns, and commit as soon as the task is done.",
            v.reason, agent.num_turns
        ),
        _ => {
            let mut fb = String::from("The previous attempt failed verification:\n");
            for c in v.checks.iter().filter(|c| !c.ok) {
                fb.push_str(&format!(
                    "- {} {} ({}):\n",
                    c.level,
                    c.name,
                    if c.timed_out {
                        "timed out".to_string()
                    } else {
                        c.exit
                            .map_or("no exit code".into(), |e| format!("exit {e}"))
                    }
                ));
                if !c.failing_tests.is_empty() {
                    fb.push_str(&format!(
                        "    failing tests: {}\n",
                        c.failing_tests.join(", ")
                    ));
                }
                for l in last_lines(&c.tail, 20).lines() {
                    fb.push_str(&format!("    {l}\n"));
                }
            }
            fb.push_str("Fix the failures, leave the tree clean, commit, and report only checks you actually ran.");
            fb
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(level: &str, name: &str, ok: bool) -> CheckResult {
        CheckResult {
            level: level.into(),
            name: name.into(),
            ok,
            exit: Some(!ok as i32),
            ..Default::default()
        }
    }

    #[test]
    fn verdict_table() {
        type Case = (
            Option<&'static str>,
            Option<&'static str>,
            Vec<CheckResult>,
            AttemptState,
            &'static str,
        );
        let cases: Vec<Case> = vec![
            (
                Some("agent timed out"),
                None,
                vec![],
                AttemptState::AgentFailed,
                "agent timed out",
            ),
            (
                Some("agent exit 1"),
                Some("q"),
                vec![c("L1", "test", true)],
                AttemptState::AgentFailed,
                "agent exit 1",
            ),
            (
                None,
                Some("which db?"),
                vec![c("L0", "a", false)],
                AttemptState::NeedsInput,
                "needs input: which db?",
            ),
            (None, None, vec![], AttemptState::Unverified, "no L1 or L2"),
            (
                None,
                None,
                vec![c("L0", "clean-tree", true)],
                AttemptState::Unverified,
                "no L1 or L2",
            ),
            (
                None,
                None,
                vec![c("L0", "clean-tree", false), c("L1", "test", true)],
                AttemptState::ChecksFailed,
                "L0 failed: clean-tree",
            ),
            (
                None,
                None,
                vec![
                    c("L0", "a", true),
                    c("L1", "fmt", true),
                    c("L1", "test", false),
                ],
                AttemptState::ChecksFailed,
                "L1 failed: test",
            ),
            (
                None,
                None,
                vec![c("L1", "test", true), c("L1", "claim:test", false)],
                AttemptState::ChecksFailed,
                "L1 failed: claim:test",
            ),
            (
                None,
                None,
                vec![c("L1", "test", true), c("L2", "task-check-1", false)],
                AttemptState::ChecksFailed,
                "L2 failed: task-check-1",
            ),
            (
                None,
                None,
                vec![c("L0", "a", true), c("L1", "test", true)],
                AttemptState::Succeeded,
                "",
            ),
            (
                None,
                None,
                vec![c("L0", "a", true), c("L2", "task-check-1", true)],
                AttemptState::Succeeded,
                "",
            ),
            (
                None,
                None,
                vec![c("L1", "a", false), c("L2", "b", false)],
                AttemptState::ChecksFailed,
                "L1 failed: a",
            ),
        ];
        for (agent, q, checks, want, reason) in cases {
            let (got, why) = decide(agent, q, &checks);
            assert_eq!(got, want, "agent={agent:?} q={q:?} checks={checks:?}");
            assert!(
                why.contains(reason),
                "want reason containing {reason:?}, got {why:?}"
            );
        }
    }

    #[test]
    fn agent_failure_reasons() {
        let ok = Outcome {
            exit_code: Some(0),
            got_result: true,
            ..Default::default()
        };
        assert_eq!(agent_failure(&ok), None);
        assert_eq!(
            agent_failure(&Outcome {
                timed_out: true,
                ..Default::default()
            })
            .as_deref(),
            Some("agent timed out")
        );
        assert_eq!(
            agent_failure(&Outcome {
                exit_code: Some(2),
                ..Default::default()
            })
            .as_deref(),
            Some("agent exit 2")
        );
        assert_eq!(
            agent_failure(&Outcome {
                exit_code: Some(0),
                ..Default::default()
            })
            .as_deref(),
            Some("agent produced no result")
        );
        assert_eq!(
            agent_failure(&Outcome {
                exit_code: Some(0),
                got_result: true,
                is_error: true,
                ..Default::default()
            })
            .as_deref(),
            Some("agent reported an error")
        );
    }

    #[test]
    fn feedback_names_failing_tests_first() {
        let v = Verdict {
            commits: 1,
            files_changed: 1,
            dirty: false,
            envelope: None,
            checks: vec![CheckResult {
                level: "L1".into(),
                name: "test".into(),
                ok: false,
                exit: Some(1),
                tail: "--- FAIL: TestA\nFAIL".into(),
                failing_tests: vec!["TestA".into()],
                ..Default::default()
            }],
            state: AttemptState::ChecksFailed,
            reason: "L1 failed: test".into(),
        };
        let fb = feedback(&v, &Outcome::default(), 30);
        assert!(fb.contains("failing tests: TestA"), "{fb}");
        assert!(fb.contains("- L1 test (exit 1)"), "{fb}");
    }
}
