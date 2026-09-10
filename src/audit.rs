//! Full visibility over a workflow run, and what to do when it fails.
//!
//! Every attempt carries `inputs` (what the step was given) and `outputs`
//! (what it produced) as JSON, next to the verdict. `diagnose` reads a task
//! and its attempts and says, from a table, what happened and what the
//! operator can do about it. It is deterministic and tested; it is the
//! difference between a failure and a failure that needs forensics.

use crate::checks::CheckResult;
use crate::store::{Attempt, AttemptState, Task, TaskState};
use serde::{Deserialize, Serialize};

/// What a step was given. Everything the agent saw or that shaped the run,
/// except the prompt text itself, which is the first line of the log.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct Inputs {
    pub workflow: String,
    pub workflow_hash: String,
    pub step: String,
    pub model: String,
    pub max_turns: i64,
    pub timeout_secs: i64,
    pub base_sha: String,
    pub start_sha: String,
    /// The feedback from the previous attempt, verbatim, if any.
    pub feedback: Option<String>,
    /// The interface handed to the coder, if any.
    pub interface: Option<String>,
    /// Refs whose namespace files were overlaid before L1.
    pub overlay_refs: Vec<String>,
    /// Whether the task's --check commands were shown to the agent.
    pub checks_shown: bool,
    pub task_checks: Vec<String>,
    pub protected: Vec<String>,
    pub namespace: Vec<String>,
    pub prompt_chars: usize,
}

/// What a step produced, beyond the verdict rows.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct Outputs {
    pub end_sha: String,
    pub changed_files: Vec<String>,
    pub dirty_files: Vec<String>,
    /// The tests step's published ref and its commit.
    pub verify_ref: Option<String>,
    /// The interface the tests step produced.
    pub interface: Option<String>,
    pub summary: String,
    pub claims: usize,
    pub checks_run: usize,
}

/// A diagnosis line: what happened, and what the operator can do.
#[derive(Debug, PartialEq, Eq)]
pub struct Diagnosis {
    pub what: String,
    pub action: String,
}

fn rows(a: &Attempt) -> Vec<CheckResult> {
    serde_json::from_str(&a.verdict_json).unwrap_or_default()
}

/// The table. Ordered from most specific to least; every terminal state
/// yields at least one line. Succeeded tasks yield none unless the push
/// failed.
pub fn diagnose(t: &Task, attempts: &[Attempt]) -> Vec<Diagnosis> {
    let mut out = Vec::new();
    let d = |what: &str, action: &str| Diagnosis {
        what: what.to_string(),
        action: action.to_string(),
    };
    let last = attempts.last();

    match t.state {
        TaskState::Succeeded => {
            if !t.pushed && t.reason.starts_with("push failed") {
                out.push(d(&t.reason, "The branch is verified but not on the remote. Fix credentials or the remote, then push the clone's branch by hand."));
            }
            return out;
        }
        TaskState::Blocked => {
            if t.reason.starts_with("needs workflow") {
                out.push(d(&t.reason, "A workflow request. Add or adjust a workflow file in <FORGE2_HOME>/workflows/ and re-add the task with --workflow."));
            } else {
                out.push(d(&t.reason, "The agent needs the operator. Re-add the task with the answer written into its text."));
            }
            return out;
        }
        TaskState::Unverified => {
            out.push(d(&t.reason, "Nothing verified the work. Declare [checks] in forge.toml or add --check commands; the branch was not pushed."));
            return out;
        }
        TaskState::Queued | TaskState::Running => return out,
        TaskState::Failed => {}
    }

    if t.reason.starts_with("task budget reached") {
        out.push(d(
            &t.reason,
            "Raise --budget for the task or per_task_usd in config.toml, or split the task.",
        ));
    }
    if t.reason.starts_with("error:") {
        out.push(d(&t.reason, "An internal error on this task, not the agent's work. Read the message; if it names the repo or git, fix that and re-add."));
    }

    // Turn and time cliffs, per step.
    for a in attempts {
        if a.state != AttemptState::AgentFailed {
            continue;
        }
        let inputs: Inputs = serde_json::from_str(&a.inputs_json).unwrap_or_default();
        // Attempts from before inputs were recorded fall back to the task's limit.
        let limit = if inputs.max_turns > 0 {
            inputs.max_turns
        } else {
            t.max_turns
        };
        if a.timed_out {
            out.push(d(
                &format!(
                    "step {} attempt {} timed out after {}s",
                    a.step, a.attempt_no, inputs.timeout_secs
                ),
                &format!(
                    "Raise timeout_secs for the {} step in the workflow file, or split the task.",
                    a.step
                ),
            ));
        } else if limit > 0 && a.num_turns >= limit {
            out.push(d(
                &format!("step {} attempt {} hit its turn limit ({} of {})", a.step, a.attempt_no, a.num_turns, limit),
                &format!("Raise max_turns for the {} step in the workflow file, or make the task smaller.", a.step),
            ));
        } else if !a.reason.is_empty() {
            out.push(d(&format!("step {} attempt {}: {}", a.step, a.attempt_no, a.reason), "The agent process failed outside Forge's rules. Read the attempt's log; if the CLI crashed, retry the task."));
        }
    }

    if let Some(a) = last
        && a.state == AttemptState::ChecksFailed
    {
        let failed: Vec<CheckResult> = rows(a).into_iter().filter(|c| !c.ok).collect();
        for c in &failed {
            let line = match (c.level.as_str(), c.name.as_str()) {
                ("L1", "red-on-base") => Some(d(
                    "the tests step wrote tests that already pass on the base commit",
                    "Either the task is already done on main, or the tests are vacuous. Check the task text; if it is real, give the tests step a clearer description of the new behavior.",
                )),
                ("L0", "clean-tree")
                | ("L0", "changes-match-git")
                | ("L0", "claims-have-evidence")
                | ("L0", "result-structured") => Some(d(
                    &format!(
                        "{} {}: the agent broke the result contract ({})",
                        c.level,
                        c.name,
                        crate::checks::last_lines(&c.tail, 2).replace('\n', " ")
                    ),
                    "Usually a one-off; a retry fixes it. If it repeats with the same model, that model is weak at the contract and the workflow should give it fewer, smaller steps.",
                )),
                ("L0", "protected-paths") => Some(d(
                    &format!(
                        "the agent changed a protected path ({})",
                        crate::checks::last_lines(&c.tail, 1)
                    ),
                    "If the task legitimately needs it, re-add with --allow-protected; otherwise the task text is steering the agent at the tests.",
                )),
                ("L0", "namespace-untouched") => Some(d(
                    "the coder created files inside the verification namespace",
                    "That is the shadow-test pattern. Re-add the task; if it repeats, the model is gaming and the task should not run unattended with it.",
                )),
                ("L0", "has-commits") => Some(d(
                    "the agent committed nothing",
                    "Read the log's last result; the agent likely explained why in its summary. Re-add with a clearer task.",
                )),
                ("L1", n) if n.starts_with("claim:") => Some(d(
                    &format!(
                        "false claim: the agent reported `{}` passed and Forge could not reproduce it",
                        &n[6..]
                    ),
                    "Treat this model as untrustworthy on this repo until it stops; do not lower the check.",
                )),
                ("L1", "setup") => Some(d(
                    "the repo's setup check failed (dependencies)",
                    "Not the agent's work. Run the setup command in a clean clone of main; fix forge.toml or the lockfile.",
                )),
                ("L1", n) => Some(d(
                    &format!(
                        "the repo's `{n}` check fails on the branch{}",
                        if c.failing_tests.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", c.failing_tests.join(", "))
                        }
                    ),
                    "Read the failing test names in the trace. More retries help if the agent was close; otherwise split the task or write it against a --check.",
                )),
                ("L2", _) => Some(d(
                    &format!(
                        "an acceptance command failed: {}",
                        crate::checks::last_lines(&c.tail, 1)
                    ),
                    "If the task text under-specified what the command checks, re-add with --show-checks or say it in words. If the command is wrong, fix the command.",
                )),
                _ => None,
            };
            if let Some(l) = line {
                out.push(l);
            }
        }
    }

    if out.is_empty() {
        out.push(d(&t.reason, "Read `forge trace` for this task; this failure has no table entry yet and deserves one."));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(state: TaskState, reason: &str) -> Task {
        Task {
            state,
            reason: reason.into(),
            ..Default::default()
        }
    }
    fn attempt(step: &str, state: AttemptState, rows: Vec<CheckResult>, inputs: Inputs) -> Attempt {
        Attempt {
            step: step.into(),
            attempt_no: 1,
            state,
            verdict_json: serde_json::to_string(&rows).unwrap(),
            inputs_json: serde_json::to_string(&inputs).unwrap(),
            ..Default::default()
        }
    }
    fn row(level: &str, name: &str) -> CheckResult {
        CheckResult {
            level: level.into(),
            name: name.into(),
            ok: false,
            ..Default::default()
        }
    }

    #[test]
    fn diagnosis_table() {
        assert!(diagnose(&task(TaskState::Succeeded, ""), &[]).is_empty());
        let b = diagnose(&task(TaskState::Blocked, "needs workflow: e2e"), &[]);
        assert!(b[0].action.contains("workflows/"));
        let q = diagnose(&task(TaskState::Blocked, "needs input: which?"), &[]);
        assert!(q[0].action.contains("answer"));
        let u = diagnose(&task(TaskState::Unverified, "no L1 or L2"), &[]);
        assert!(u[0].action.contains("--check"));

        let mut a = attempt(
            "tests",
            AttemptState::AgentFailed,
            vec![],
            Inputs {
                max_turns: 40,
                ..Default::default()
            },
        );
        a.num_turns = 41;
        let f = diagnose(
            &task(TaskState::Failed, "agent exit 1 (after 1 attempt(s))"),
            &[a],
        );
        assert!(f[0].what.contains("turn limit"), "{f:?}");
        assert!(f[0].action.contains("max_turns for the tests step"));

        let red = attempt(
            "tests",
            AttemptState::ChecksFailed,
            vec![row("L1", "red-on-base")],
            Inputs::default(),
        );
        let f = diagnose(&task(TaskState::Failed, "L1 failed: red-on-base"), &[red]);
        assert!(f[0].what.contains("already pass on the base"));

        let claim = attempt(
            "code",
            AttemptState::ChecksFailed,
            vec![row("L1", "test"), row("L1", "claim:test")],
            Inputs::default(),
        );
        let f = diagnose(
            &task(TaskState::Failed, "L1 failed: test, claim:test"),
            &[claim],
        );
        assert!(f.iter().any(|d| d.what.starts_with("false claim")), "{f:?}");

        let budget = diagnose(
            &task(
                TaskState::Failed,
                "task budget reached: $2.01 of $2.00 after 3 attempt(s)",
            ),
            &[],
        );
        assert!(budget[0].action.contains("per_task_usd"));

        let unknown = diagnose(&task(TaskState::Failed, "something new"), &[]);
        assert!(unknown[0].action.contains("forge trace"));
    }
}
