//! Verification: claims are verified, not believed. Three levels, each run
//! by Forge after the agent has exited, each a row in the verdict.
//!
//! - L0: is the result consistent with git? A structured result exists,
//!   the tree is clean, there is at least one commit, `forge.toml`,
//!   protected paths, and the verification namespace are untouched, every
//!   claim has evidence. The result's `changes[]` is never taken from the
//!   model: `derive_changes` overwrites it from git before this level
//!   runs, for every provider (`Rule::ChangesFromGit`; the retired
//!   `Rule::ChangesMatchGit` held the model to its own list instead, and
//!   only survives so `Rule::parse` and `audit::rule_diagnosis` still
//!   read a verdict written before this changed).
//! - L1: do the repository's declared checks pass, read from the trusted
//!   base commit and run by Forge in the sandbox, with the verification
//!   namespace overlaid from the trusted refs? And for every check the
//!   agent reported as passed, did Forge's run pass too? A claimed pass
//!   Forge cannot reproduce is the canonical false claim.
//! - L2: do the task's own acceptance commands pass?
//!
//! A level runs only if the one before it passed. The overlay is removed
//! afterwards so the next attempt starts blind. `decide` maps the rows to a
//! terminal state and is a pure function with a table test.
//!
//! The tests step has its own verdict: only the namespace changed, and the
//! tests fail on the base commit.

use crate::agent::Outcome;
use crate::checks::{CheckResult, last_lines};
use crate::config::Config;
use crate::envelope::{Envelope, Kind};
use crate::executor::Execution;
use crate::report::{Event, Reporter};
use crate::store::AttemptState;
use std::path::Path;

mod directive;
mod fault;
mod l0_checks;
mod overlay;
mod recovery;
pub mod review;
mod run_checks;

pub use directive::{verify_directive, verify_integration, verify_operation};
pub use fault::tests_fault;
pub use overlay::{overlay, overlay_label, remove_overlay};

#[cfg(test)]
mod test_support;

pub struct Subject<'a> {
    pub task_id: i64,
    /// The registered checkout: where the trusted verify refs live.
    pub repo: &'a Path,
    pub worktree: &'a Path,
    pub base_sha: &'a str,
    pub start_sha: &'a str,
    /// The branch checked out in `worktree`: the task's, or at a joint
    /// integration the integration branch. Told to every check as
    /// `FORGE_BRANCH`.
    pub branch: &'a str,
    pub cfg: &'a Config,
    pub task_checks: &'a [String],
    /// The directive's write scope; empty means anywhere not otherwise forbidden.
    pub paths: &'a [String],
    pub allow_protected: bool,
    /// Refs whose namespace files are overlaid before L1: `forge-verify`
    /// for standing suites, `verify/<id>` for the task's own tests.
    pub overlay_refs: &'a [String],
    /// The base branch's current tip, once landing found the branch behind
    /// it. When HEAD contains it, the branch is measured against it: the
    /// merge carried the base's changes, the agent did not make them.
    pub pending_main: Option<&'a str>,
    pub sandbox: Option<&'a Execution>,
    pub report: &'a Reporter,
    /// Where a failed L1/L2 check's whole output is written
    /// (`run_one_recorded`): `Forge::paths.logs`, the same directory the
    /// attempt's own transcript lives in, so `forge show` names a path
    /// that outlives the worktree.
    pub logs_dir: &'a Path,
    /// The tests contract's scratch directory for the red-on-base run;
    /// created and removed by the verdict. Other contracts leave it None.
    pub scratch: Option<&'a Path>,
    /// Whether a `Contract::Plan` directive's summary is judged as a
    /// repository file plan (`plan-substantive`, `plan-names-real-paths`):
    /// true for `investigate`, false for `interview`, whose summary is
    /// either a question, a brief, or a plain sentence ending the
    /// conversation, none of which is a file plan.
    pub plan_rows: bool,
}

impl Subject<'_> {
    /// The task's facts as environment for every check run on its tree:
    /// the same list an operation gets (`operation::task_facts`).
    fn facts(&self) -> Vec<(String, String)> {
        crate::operation::task_facts(self.task_id, self.base_sha, self.start_sha, self.branch)
    }
}

/// What git says about the branch at verdict time.
#[derive(Debug, Default, Clone)]
pub struct GitFacts {
    pub commits: i64,
    /// Every path changed since the base (or the merged base).
    pub changed: Vec<String>,
    /// What this attempt itself changed, since it started.
    pub changed_now: Vec<String>,
    pub dirty: Vec<String>,
}

/// What every step's L0 shares, computed once.
pub struct Common {
    pub rows: Vec<CheckResult>,
    pub envelope: Option<Envelope>,
    pub question: Option<(Kind, String)>,
    pub facts: GitFacts,
}

pub struct Verdict {
    pub commits: i64,
    pub files_changed: i64,
    pub dirty: bool,
    pub envelope: Option<Envelope>,
    pub checks: Vec<CheckResult>,
    pub state: AttemptState,
    pub reason: String,
    /// Set when a code attempt failed only checks `[checks.fixable]` names
    /// and the engine ran their fix commands before deciding the verdict
    /// (see `try_known_fix`); `None` when no fix was attempted, whether
    /// because nothing failed or because the failure was not fixable.
    pub known_fix: Option<KnownFix>,
}

/// What running `[checks.fixable]`'s commands did, for the "known-fix"
/// operation row `engine::run_task` records on the attempt.
#[derive(Debug)]
pub struct KnownFix {
    /// The checks whose fix command ran, sorted.
    pub checks: Vec<String>,
    /// The commit the fix produced, if the fix commands changed anything.
    pub commit: Option<String>,
    /// `git diff --shortstat` between the tree before the fix and the fix
    /// commit; empty when nothing was committed.
    pub diff_stat: String,
    /// Whether every fix command exited 0 and something was committed.
    pub ok: bool,
}

impl Verdict {
    /// A verdict opened on the git facts, with no rows yet and no
    /// decision; `settle` closes it. The only way to make one.
    pub fn open(facts: &GitFacts) -> Verdict {
        Verdict {
            commits: facts.commits,
            files_changed: facts.changed.len() as i64,
            dirty: !facts.dirty.is_empty(),
            envelope: None,
            checks: Vec::new(),
            state: AttemptState::Running,
            reason: String::new(),
            known_fix: None,
        }
    }

    /// Decide the state and the reason from what the verdict holds.
    /// `verifies` says whether this contract runs checks of its own; one
    /// that does not is judged by its rows alone.
    pub fn settle(
        &mut self,
        agent_failure: Option<&str>,
        question: Option<(Kind, &str)>,
        verifies: bool,
    ) {
        let (state, reason) = decide(agent_failure, question, &self.checks, verifies);
        self.state = state;
        self.reason = reason;
    }
}

/// Every row the kernel writes about an attempt's own conduct, by name.
/// The names are what the trace, the events, the audit and the tests
/// carry; adding a rule here makes the audit's table refuse to compile
/// until it has a line for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    ResultStructured,
    SuiteNamesAHiddenTest,
    CleanTree,
    NoStrayFiles,
    ConfigUntouched,
    HasCommits,
    ChangesMatchGit,
    ChangesFromGit,
    ClaimsHaveEvidence,
    CandidateUnchanged,
    ProtectedPaths,
    PathsInScope,
    NamespaceUntouched,
    NamespaceOnly,
    InterfaceDescribed,
    RedOnBase,
    NoWrites,
    ExecutedSomething,
    Untouched,
    PlanSubstantive,
    PlanNamesRealPaths,
    CitesRealThings,
    Substantive,
    SupersedesWithALandedTask,
    ReproductionSelfContained,
}

impl Rule {
    pub const ALL: [Rule; 25] = [
        Rule::ResultStructured,
        Rule::SuiteNamesAHiddenTest,
        Rule::CleanTree,
        Rule::NoStrayFiles,
        Rule::ConfigUntouched,
        Rule::HasCommits,
        Rule::ChangesMatchGit,
        Rule::ChangesFromGit,
        Rule::ClaimsHaveEvidence,
        Rule::CandidateUnchanged,
        Rule::ProtectedPaths,
        Rule::PathsInScope,
        Rule::NamespaceUntouched,
        Rule::NamespaceOnly,
        Rule::InterfaceDescribed,
        Rule::RedOnBase,
        Rule::NoWrites,
        Rule::ExecutedSomething,
        Rule::Untouched,
        Rule::PlanSubstantive,
        Rule::PlanNamesRealPaths,
        Rule::CitesRealThings,
        Rule::Substantive,
        Rule::SupersedesWithALandedTask,
        Rule::ReproductionSelfContained,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Rule::ResultStructured => "result-structured",
            Rule::SuiteNamesAHiddenTest => "suite-names-a-hidden-test",
            Rule::CleanTree => "clean-tree",
            Rule::NoStrayFiles => "no-stray-files",
            Rule::ConfigUntouched => "forge.toml-untouched",
            Rule::HasCommits => "has-commits",
            Rule::ChangesMatchGit => "changes-match-git",
            Rule::ChangesFromGit => "changes-from-git",
            Rule::ClaimsHaveEvidence => "claims-have-evidence",
            Rule::CandidateUnchanged => "candidate-unchanged",
            Rule::ProtectedPaths => "protected-paths",
            Rule::PathsInScope => "paths-in-scope",
            Rule::NamespaceUntouched => "namespace-untouched",
            Rule::NamespaceOnly => "namespace-only",
            Rule::InterfaceDescribed => "interface-described",
            Rule::RedOnBase => "red-on-base",
            Rule::NoWrites => "no-writes",
            Rule::ExecutedSomething => "executed-something",
            Rule::Untouched => "untouched",
            Rule::PlanSubstantive => "plan-substantive",
            Rule::PlanNamesRealPaths => "plan-names-real-paths",
            Rule::CitesRealThings => "cites-real-things",
            Rule::Substantive => "substantive",
            Rule::SupersedesWithALandedTask => "supersedes-with-a-landed-task",
            Rule::ReproductionSelfContained => "reproduction-self-contained",
        }
    }

    /// Which level the row is reported at: `red-on-base` runs the suite,
    /// so it is L1; `executed-something` and `changes-from-git` are notes
    /// that never decide.
    pub fn level(self) -> &'static str {
        match self {
            Rule::RedOnBase => "L1",
            Rule::ExecutedSomething | Rule::ChangesFromGit => "note",
            _ => "L0",
        }
    }

    pub fn parse(name: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.name() == name)
    }
}

/// A rule's row: level and name from the registry, the detail kept only
/// when it failed.
pub(crate) fn l0(rule: Rule, ok: bool, detail: String) -> CheckResult {
    CheckResult {
        level: rule.level().into(),
        name: rule.name().into(),
        ok,
        tail: if ok { String::new() } else { detail },
        ..Default::default()
    }
}

/// One check row, as an event.
pub fn emit_check(report: &Reporter, task_id: i64, c: &CheckResult) {
    report.emit(
        task_id,
        Event::Check {
            level: &c.level,
            name: &c.name,
            ok: c.ok,
            ms: c.ms,
            tail: &last_lines(&c.tail, 20),
        },
    );
}

fn emit_rows(report: &Reporter, task_id: i64, rows: &[CheckResult]) {
    for c in rows {
        emit_check(report, task_id, c);
    }
}

/// Whether an attempt's recorded rows include at least one L1 check and
/// every L1 row passed: the repository's checks vouch for the tree, even
/// when the attempt itself did not settle (a question, a review
/// demotion). Used to let a retry start from a needs-input attempt's
/// branch, and to let the supervisor land one, instead of treating every
/// question as unverified.
pub fn l1_all_passed(checks: &[CheckResult]) -> bool {
    let l1: Vec<&CheckResult> = checks.iter().filter(|c| c.level == "L1").collect();
    !l1.is_empty() && l1.iter().all(|c| c.ok)
}

/// The verdict table. Pure: the same rows always give the same answer.
/// `question` is (kind, text). `verifies` is whether the contract runs
/// checks of its own: one that does not (review, plan) is judged by its
/// rows alone, so nothing to object to is a pass rather than unverified.
pub fn decide(
    agent_failure: Option<&str>,
    question: Option<(Kind, &str)>,
    checks: &[CheckResult],
    verifies: bool,
) -> (AttemptState, String) {
    if let Some(why) = agent_failure {
        return (AttemptState::AgentFailed, why.to_string());
    }
    if let Some((kind, q)) = question {
        return (AttemptState::NeedsInput, format!("{}: {q}", kind.label()));
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
    if verifies && !verified {
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
            Option<(Kind, &'static str)>,
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
                Some((Kind::Question, "q")),
                vec![c("L1", "test", true)],
                AttemptState::AgentFailed,
                "agent exit 1",
            ),
            (
                None,
                Some((Kind::Question, "which db?")),
                vec![c("L0", "a", false)],
                AttemptState::NeedsInput,
                "needs input: which db?",
            ),
            (
                None,
                Some((Kind::Workflow, "need e2e")),
                vec![],
                AttemptState::NeedsInput,
                "needs workflow: need e2e",
            ),
            (
                None,
                Some((Kind::Review, "off by one")),
                vec![c("L1", "t", true)],
                AttemptState::NeedsInput,
                "review demoted: off by one",
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
            let (got, why) = decide(agent, q, &checks, true);
            assert_eq!(got, want, "agent={agent:?} q={q:?} checks={checks:?}");
            assert!(
                why.contains(reason),
                "want reason containing {reason:?}, got {why:?}"
            );
        }
    }

    #[test]
    fn l1_all_passed_needs_at_least_one_l1_row_and_none_failing() {
        assert!(!l1_all_passed(&[]), "no L1 rows at all is not a pass");
        assert!(!l1_all_passed(&[c("L0", "clean-tree", true)]));
        assert!(l1_all_passed(&[
            c("L0", "clean-tree", true),
            c("L1", "test", true)
        ]));
        assert!(!l1_all_passed(&[
            c("L1", "test", true),
            c("L1", "lint", false)
        ]));
    }

    #[test]
    fn a_contract_that_runs_no_checks_passes_on_its_rows_alone() {
        let rows = vec![c("L0", "clean-tree", true), c("L0", "no-writes", true)];
        assert_eq!(
            decide(None, None, &rows, false),
            (AttemptState::Succeeded, String::new())
        );
        assert_eq!(decide(None, None, &rows, true).0, AttemptState::Unverified);
        let bad = vec![c("L0", "untouched", false)];
        assert_eq!(
            decide(None, None, &bad, false).0,
            AttemptState::ChecksFailed
        );
    }

    #[test]
    fn every_rule_name_round_trips_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for r in Rule::ALL {
            assert_eq!(Rule::parse(r.name()), Some(r));
            assert!(seen.insert(r.name()), "duplicate rule name {}", r.name());
        }
        assert_eq!(Rule::RedOnBase.level(), "L1");
        assert_eq!(Rule::ExecutedSomething.level(), "note");
        assert_eq!(Rule::CleanTree.level(), "L0");
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
            known_fix: None,
        };
        let fb = feedback(&v, &Outcome::default(), 30);
        assert!(fb.contains("failing tests: TestA"), "{fb}");
        assert!(fb.contains("- L1 test (exit 1)"), "{fb}");
    }
}
