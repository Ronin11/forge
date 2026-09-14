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
    /// The CLI session this attempt continued, when it resumed a capped one.
    #[serde(default)]
    pub resumed: Option<String>,
    /// The journal the agent was shown: every earlier attempt in this piece
    /// of work, what it said it did, and what the kernel found. Verbatim.
    #[serde(default)]
    pub journal: Option<String>,
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
    /// Tool calls before the first edit or write; `None` when the attempt
    /// never edited. Exploration, measured.
    #[serde(default)]
    pub first_edit_call: Option<i64>,
    /// What the attempt ran: tools, shell command families, files read,
    /// each with calls and time.
    #[serde(default)]
    pub tools: Option<crate::tools::Tools>,
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

    // Cost anti-patterns: facts about what an attempt ran and how far it
    // got, independent of whether the task ultimately succeeded. A task
    // can land and still have burned turns the way it shouldn't have.
    for a in attempts {
        let cost = a.cost_usd.map_or("-".to_string(), |c| format!("${c:.4}"));
        let inputs: Inputs = serde_json::from_str(&a.inputs_json).unwrap_or_default();
        let limit = if inputs.max_turns > 0 {
            inputs.max_turns
        } else {
            t.max_turns
        };
        if !a.timed_out && limit > 0 && a.num_turns >= limit && a.dirty {
            out.push(d(
                &format!(
                    "step {} attempt {} was capped with a dirty tree ({} of {} turns, {})",
                    a.step, a.attempt_no, a.num_turns, limit, cost
                ),
                "Split the task into smaller pieces so an attempt can finish, and commit, inside its turn budget.",
            ));
        }

        let outputs: Outputs = serde_json::from_str(&a.outputs_json).unwrap_or_default();
        if let Some(tools) = &outputs.tools {
            for (file, count) in &tools.reads {
                if *count >= 4 {
                    out.push(d(
                        &format!(
                            "step {} attempt {} read the same file {} times: {} ({})",
                            a.step, a.attempt_no, count, file, cost
                        ),
                        "Give it the journal so it does not rediscover what an earlier attempt already read.",
                    ));
                }
            }
        }
        if let Some(fe) = outputs.first_edit_call
            && fe >= 15
        {
            out.push(d(
                &format!(
                    "step {} attempt {} explored {} calls before the first edit ({})",
                    a.step, a.attempt_no, fe, cost
                ),
                "Add the file to context up front instead of making the agent explore to find it.",
            ));
        }
    }

    match t.state {
        TaskState::Succeeded => {
            if !t.pushed && t.reason.starts_with("push failed") {
                out.push(d(&t.reason, "The branch is verified but not on the remote. Fix credentials or the remote, then push the clone's branch by hand."));
            }
            return out;
        }
        TaskState::Blocked => {
            if t.reason.starts_with("review demoted") {
                out.push(d(&t.reason, "The reviewer demonstrated a defect. The branch passed the checks and is pushed; look at the command it names, then either fix by re-adding the task or, if the reviewer is wrong, note it and merge. Reviewer precision is measured from what you decide here."));
                return out;
            }
            if t.reason.starts_with("waits on task") {
                out.push(d(&t.reason, "A task this one was queued --after ended without landing. Fix that one, then `forge retry <its id> --chain` re-queues it and everything waiting on it."));
                return out;
            }
            if t.reason.starts_with("needs suite") {
                out.push(d(&t.reason, "A hidden test on forge-verify contradicts this task, and the coder may not edit it. Decide which is right: if the task is, change the test on the forge-verify branch and re-add the task; if the test is, rewrite the task."));
                return out;
            }
            if t.reason.starts_with("needs workflow") {
                out.push(d(&t.reason, "A workflow request. Add or adjust a workflow file in <FORGE2_HOME>/workflows/ and re-add the task with --workflow."));
            } else {
                out.push(d(&t.reason, "The agent needs the operator. Re-add the task with the answer written into its text."));
            }
            return out;
        }
        TaskState::Unverified => {
            if t.reason.starts_with("ran out of turns after committing") {
                out.push(d(&t.reason, "The code passed the checks but the coder never returned a result, so nothing vouches for what it did. Read the branch's diff; merge it if it is the task, or retry with more turns."));
            } else if t.reason.starts_with("review could not finish") {
                out.push(d(&t.reason, "The code step verified the branch; only the reviewer failed to reach a verdict, usually its turn limit. Review the branch yourself, or raise max_turns on the review action and run the task again."));
            } else {
                out.push(d(&t.reason, "Nothing verified the work. Declare [checks] in forge.toml or add --check commands; the branch was not pushed."));
            }
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
    if t.reason.starts_with("landing failed") {
        out.push(d(
            &t.reason,
            "The branch verified on its own but could not land: the base kept moving, or with the base merged in a conflict or a failing check outlived the coder's attempts or budget. The branch is pushed; read the integrate rows in the trace, then merge by hand or re-add the task.",
        ));
    }
    if t.reason.starts_with("check ") && t.reason.contains("inside the verification namespace") {
        out.push(d(
            &t.reason,
            "The hidden tests broke a repository check on the implementer's tree, and the test author could not fix them within its attempts. Read the check's output in the trace: the tests must compile and lint on their own, since the implementer never sees them.",
        ));
    }
    if let Some(rest) = t.reason.strip_prefix("operation ") {
        let name = rest.split_whitespace().next().unwrap_or("?");
        let timed_out = t.reason.contains("timed out after");
        let after_change = ["L0 failed", "L1 failed", "L2 failed"]
            .iter()
            .any(|m| t.reason.contains(m));
        let verifies = rest.contains("(verifies)");
        out.push(d(
            &t.reason,
            &if verifies {
                format!("The `{name}` verification kept failing after every attempt the coder had. Read its output in the trace; if the suite is right, the task needs more attempts or a smaller scope; if the suite is wrong, fix it on the forge-verify branch.")
            } else if timed_out {
                format!("Operation `{name}` hit its timeout. Raise timeout_secs in workflows/actions/{name}.toml or make the command faster; operations are deterministic, so a retry would time out again.")
            } else if after_change {
                format!("Operation `{name}` changed the tree and the result failed verification; its commit is on the branch for inspection. Fix what the operation does in workflows/actions/{name}.toml, or the check it broke, and re-add the task.")
            } else {
                format!("Operation `{name}` is deterministic, so a retry would fail the same way. Fix the command in workflows/actions/{name}.toml, or, for a `check` operation, the repository's own check on its base branch; then re-add the task.")
            },
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
        let r = diagnose(&task(TaskState::Blocked, "review demoted: off by one"), &[]);
        assert!(r[0].action.contains("reviewer"), "{r:?}");
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

        let opf = diagnose(
            &task(TaskState::Failed, "operation stamp failed: boom"),
            &[],
        );
        assert!(opf[0].action.contains("actions/stamp.toml"), "{opf:?}");
        let opt = diagnose(
            &task(
                TaskState::Failed,
                "operation slowop failed: timed out after 1s",
            ),
            &[],
        );
        assert!(opt[0].action.contains("timeout_secs"), "{opt:?}");

        let unknown = diagnose(&task(TaskState::Failed, "something new"), &[]);
        assert!(unknown[0].action.contains("forge trace"));
    }

    #[test]
    fn capped_with_a_dirty_tree_is_a_cost_antipattern() {
        let mut a = attempt(
            "code",
            AttemptState::AgentFailed,
            vec![],
            Inputs {
                max_turns: 40,
                ..Default::default()
            },
        );
        a.num_turns = 40;
        a.dirty = true;
        a.cost_usd = Some(1.5);
        let out = diagnose(
            &task(TaskState::Failed, "agent exit 1 (after 1 attempt(s))"),
            &[a],
        );
        let row = out
            .iter()
            .find(|d| d.what.contains("capped with a dirty tree"))
            .unwrap_or_else(|| panic!("{out:?}"));
        assert!(row.what.contains("code attempt 1"), "{row:?}");
        assert!(row.what.contains("$1.5000"), "{row:?}");
        assert!(row.action.contains("Split the task"), "{row:?}");

        // A clean tree at the cap is not the anti-pattern.
        let mut clean = attempt(
            "code",
            AttemptState::AgentFailed,
            vec![],
            Inputs {
                max_turns: 40,
                ..Default::default()
            },
        );
        clean.num_turns = 40;
        clean.dirty = false;
        let out = diagnose(&task(TaskState::Failed, "agent exit 1"), &[clean]);
        assert!(
            !out.iter()
                .any(|d| d.what.contains("capped with a dirty tree")),
            "{out:?}"
        );
    }

    #[test]
    fn repeated_reads_are_a_cost_antipattern() {
        let mut tools = crate::tools::Tools::default();
        tools.reads.insert("src/a.rs".into(), 4);
        tools.reads.insert("src/b.rs".into(), 2);
        let outputs = Outputs {
            tools: Some(tools),
            ..Default::default()
        };
        let mut a = attempt("code", AttemptState::Succeeded, vec![], Inputs::default());
        a.outputs_json = serde_json::to_string(&outputs).unwrap();
        a.cost_usd = Some(0.42);
        let out = diagnose(&task(TaskState::Succeeded, ""), &[a]);
        let row = out
            .iter()
            .find(|d| d.what.contains("read the same file"))
            .unwrap_or_else(|| panic!("{out:?}"));
        assert!(row.what.contains("4 times"), "{row:?}");
        assert!(row.what.contains("src/a.rs"), "{row:?}");
        assert!(!row.what.contains("src/b.rs"), "{row:?}");
        assert!(row.action.contains("journal"), "{row:?}");
    }

    #[test]
    fn heavy_exploration_before_the_first_edit_is_a_cost_antipattern() {
        let outputs = Outputs {
            first_edit_call: Some(15),
            ..Default::default()
        };
        let mut a = attempt("code", AttemptState::Succeeded, vec![], Inputs::default());
        a.outputs_json = serde_json::to_string(&outputs).unwrap();
        let out = diagnose(&task(TaskState::Succeeded, ""), &[a]);
        let row = out
            .iter()
            .find(|d| d.what.contains("explored"))
            .unwrap_or_else(|| panic!("{out:?}"));
        assert!(row.what.contains("15 calls"), "{row:?}");
        assert!(row.action.contains("context"), "{row:?}");

        // Below the threshold, it is unremarkable.
        let below = Outputs {
            first_edit_call: Some(14),
            ..Default::default()
        };
        let mut a = attempt("code", AttemptState::Succeeded, vec![], Inputs::default());
        a.outputs_json = serde_json::to_string(&below).unwrap();
        let out = diagnose(&task(TaskState::Succeeded, ""), &[a]);
        assert!(!out.iter().any(|d| d.what.contains("explored")), "{out:?}");
    }

    #[test]
    fn every_reason_the_engine_can_emit_has_a_diagnosis() {
        // The anti-fragility rule: no terminal state that a human must
        // diagnose by hand. Every reason shape the engine produces maps to
        // a what and an action.
        let cases = [
            (
                TaskState::Failed,
                "L0 failed: clean-tree (after 1 attempt(s))",
            ),
            (TaskState::Failed, "L1 failed: test (after 2 attempt(s))"),
            (
                TaskState::Failed,
                "L2 failed: acceptance (after 1 attempt(s))",
            ),
            (TaskState::Failed, "agent exit 1 (after 1 attempt(s))"),
            (TaskState::Failed, "agent timed out (after 1 attempt(s))"),
            (
                TaskState::Failed,
                "agent produced no result (after 1 attempt(s))",
            ),
            (TaskState::Failed, "operation setup failed: exit 1"),
            (
                TaskState::Failed,
                "operation needs-extra (verifies) failed after 1 attempt(s): extra.txt is missing",
            ),
            (
                TaskState::Failed,
                "check lint failed inside the verification namespace after 1 tests attempt(s): x",
            ),
            (
                TaskState::Failed,
                "landing failed: fast-forward of main rejected",
            ),
            (
                TaskState::Failed,
                "landing failed after 2 attempt(s): main moved; conflicts in a.ts",
            ),
            (
                TaskState::Failed,
                "task budget reached: $2.0100 of $2.00 after 3 attempt(s)",
            ),
            (TaskState::Succeeded, "push failed: no route"),
            (TaskState::Blocked, "review demoted: off by one"),
            (TaskState::Blocked, "needs workflow: no e2e step"),
            (
                TaskState::Blocked,
                "needs suite: stars-tier.test.ts asserts stars are never consumed",
            ),
            (TaskState::Blocked, "needs input: which db?"),
            (
                TaskState::Blocked,
                "waits on task 14 (failed: L1 failed: test)",
            ),
            (TaskState::Unverified, "no L1 or L2"),
            (
                TaskState::Unverified,
                "ran out of turns after committing; the checks pass but no result was returned, so the branch goes to a human",
            ),
            (
                TaskState::Failed,
                "ran out of turns after committing; the checks fail: L1 failed: test (after 2 attempt(s))",
            ),
            (
                TaskState::Unverified,
                "review could not finish (agent exit 1); the branch verified at the code step and goes to human review (after 2 attempt(s))",
            ),
        ];
        for (state, reason) in cases {
            let mut t = task(state, reason);
            t.pushed = false;
            let out = diagnose(&t, &[]);
            assert!(!out.is_empty(), "no diagnosis for {state:?} {reason:?}");
            assert!(!out[0].action.is_empty(), "no action for {reason:?}");
        }
    }
}
