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
use crate::checks::{CheckResult, last_lines, run_one};
use crate::config::{Config, is_protected};
use crate::envelope::{self, Change, Envelope, Kind};
use crate::executor::Execution;
use crate::report::{Event, Reporter};
use crate::store::AttemptState;
use crate::workflows::Contract;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Overlay refs for a human: a pinned forge-verify commit reads as
/// `forge-verify@<sha8>`, everything else as itself.
pub fn overlay_label(refs: &[String]) -> String {
    refs.iter()
        .map(|r| {
            if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) {
                format!("forge-verify@{}", &r[..8])
            } else {
                r.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

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
}

impl Rule {
    pub const ALL: [Rule; 24] = [
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

fn in_namespace(namespace: &[String], path: &str) -> bool {
    namespace.iter().any(|d| path.starts_with(d.as_str()))
}

/// The report's `changes[]`, filled from git instead of the model: every
/// path this attempt added, modified, deleted or renamed since it started,
/// read the same way `changes_match_git` reconciles a report against git.
/// A rename is split into its two endpoints, so it reads like any other
/// add and delete a model would have written by hand.
async fn derive_changes(wt: &Path, start_sha: &str) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for gc in crate::git::changed_with_status(wt, start_sha, "HEAD").await? {
        match gc {
            crate::git::GitChange::Added(path) => changes.push(Change {
                path,
                kind: "added".into(),
                summary: String::new(),
            }),
            crate::git::GitChange::Modified(path) => changes.push(Change {
                path,
                kind: "modified".into(),
                summary: String::new(),
            }),
            crate::git::GitChange::Deleted(path) => changes.push(Change {
                path,
                kind: "deleted".into(),
                summary: String::new(),
            }),
            crate::git::GitChange::Renamed { from, to } => {
                changes.push(Change {
                    path: to.clone(),
                    kind: "modified".into(),
                    summary: format!("moved from {from}"),
                });
                changes.push(Change {
                    path: from,
                    kind: "deleted".into(),
                    summary: format!("moved to {to}"),
                });
            }
        }
    }
    Ok(changes)
}

/// Only net additions count: a scratch file removed before the attempt
/// finishes is harmless, and existing backup files are not this attempt's.
async fn no_stray_files(wt: &Path, start_sha: &str) -> Result<CheckResult> {
    let stray: Vec<String> = crate::git::changed_with_status(wt, start_sha, "HEAD")
        .await?
        .into_iter()
        .filter_map(|change| match change {
            crate::git::GitChange::Added(path) => Some(path),
            crate::git::GitChange::Renamed { to, .. } => Some(to),
            _ => None,
        })
        .filter(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            [".bak", ".backup", ".orig", ".rej", "~"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
                || ["temp_", "tmp_", "scratch_"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .collect();
    Ok(l0(
        Rule::NoStrayFiles,
        stray.is_empty(),
        format!(
            "stray files added during this attempt: {}. Delete them.",
            stray.join(", ")
        ),
    ))
}

/// What every step's L0 shares: git facts, the envelope, the rows that do
/// not depend on the step, and the question if the agent stopped.
pub async fn common_l0(s: &Subject<'_>, agent: &Outcome) -> Result<Common> {
    let (worktree, base_sha, start_sha, pending_main, cfg, report, task_id) = (
        s.worktree,
        s.base_sha,
        s.start_sha,
        s.pending_main,
        s.cfg,
        s.report,
        s.task_id,
    );
    // A branch that merged the moved base is measured from there.
    let merged_main = match pending_main {
        Some(m) if crate::git::is_ancestor(worktree, m, "HEAD").await => Some(m),
        _ => None,
    };
    let base_now = merged_main.unwrap_or(base_sha);
    let commits = crate::git::count_commits(worktree, base_now).await?;
    let changed = crate::git::changed_paths(worktree, base_now).await?;
    // What this attempt changed: since it started, not since base, so a
    // retry that adds nothing reports nothing and is right. An attempt that
    // merged the base in is credited with what it resolved, not with what
    // the merge carried.
    let changed_this_attempt = match merged_main {
        Some(m) if !crate::git::is_ancestor(worktree, m, start_sha).await => {
            crate::git::net_changes(worktree, base_sha, start_sha, m, "HEAD").await?
        }
        _ => crate::git::changed_paths(worktree, start_sha).await?,
    };
    let dirty = crate::git::dirty_paths(worktree).await?;
    report.emit(
        task_id,
        Event::GitCounted {
            commits,
            files: changed.len() as i64,
            dirty: !dirty.is_empty(),
        },
    );
    let mut rows = Vec::new();
    let parsed = envelope::parse(agent.structured.as_deref(), &agent.result_text);
    let mut env = match &parsed {
        Ok(Some(e)) => Some(e.clone()),
        _ => None,
    };
    rows.push(l0(
        Rule::ResultStructured,
        env.is_some(),
        match &parsed {
            Ok(None) => "the agent produced no structured result".into(),
            Err(e) => format!("the structured result does not fit the contract: {e}"),
            Ok(Some(_)) => String::new(),
        },
    ));
    let mut question = env
        .as_ref()
        .and_then(|e| e.needs_input.as_ref())
        .map(|q| (q.kind, q.question.clone()));
    // A suite exit is for a test the agent may not change: one under the
    // verification namespace or a protected path, named in `path`. Naming
    // a visible test, or none, is not a reason to stop; the step goes on
    // with that said.
    if let Some((kind, _)) = &question
        && *kind == Kind::Suite
    {
        let path = env
            .as_ref()
            .and_then(|e| e.needs_input.as_ref())
            .map(|q| q.path.trim().to_string())
            .unwrap_or_default();
        let hidden = !path.is_empty()
            && (in_namespace(&cfg.namespace, &path)
                || crate::config::is_protected(&cfg.protected, &path));
        if !hidden {
            rows.push(l0(
                Rule::SuiteNamesAHiddenTest,
                false,
                if path.is_empty() {
                    format!("a suite exit must set `path` to the test file it objects to, under {} or a protected path", cfg.namespace.join(", "))
                } else {
                    format!(
                        "a suite exit must name a test under {} or a protected path; `{path}` is a visible test, the implementer's to change. Finish the step and say in the summary which tests must change and why.",
                        cfg.namespace.join(", ")
                    )
                },
            ));
            question = None;
        }
    }
    rows.push(l0(
        Rule::CleanTree,
        dirty.is_empty(),
        format!("uncommitted: {}", dirty.join(", ")),
    ));
    rows.push(no_stray_files(worktree, start_sha).await?);
    let touched = changed
        .iter()
        .chain(dirty.iter())
        .any(|p| p == cfg.config_path.as_str());
    rows.push(l0(
        Rule::ConfigUntouched,
        !touched,
        format!("the attempt modified {}", cfg.config_path),
    ));
    rows.push(l0(
        Rule::HasCommits,
        commits > 0,
        "no commits on the branch".into(),
    ));
    if let Some(e) = &mut env {
        // The model is never held to its own list of changes: git says
        // what changed, always (see docs/CHECKS.md and the retired
        // `changes-match-git` rule below). A weak model can commit real
        // work and still misreport what it touched, and holding the
        // report against git failed the attempt for a mistake in the
        // report, not the work.
        e.changes = derive_changes(worktree, start_sha).await?;
        rows.push(l0(Rule::ChangesFromGit, true, String::new()));
        let bare: Vec<&str> = e
            .claims
            .iter()
            .filter(|c| c.evidence.trim().is_empty())
            .map(|c| c.claim.as_str())
            .collect();
        rows.push(l0(
            Rule::ClaimsHaveEvidence,
            bare.is_empty(),
            format!("claims without evidence: {}", bare.join("; ")),
        ));
    }
    Ok(Common {
        rows,
        envelope: env,
        question,
        facts: GitFacts {
            commits,
            changed,
            changed_now: changed_this_attempt,
            dirty,
        },
    })
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

/// Overlay the namespace files from each trusted ref into the tree.
/// Returns the files placed, for removal afterwards.
pub async fn overlay(
    repo: &Path,
    refs: &[String],
    namespace: &[String],
    dest: &Path,
) -> Result<Vec<PathBuf>> {
    let mut placed = Vec::new();
    if namespace.is_empty() {
        return Ok(placed);
    }
    for r in refs {
        let files = crate::git::ls_tree(repo, r, namespace).await?;
        crate::git::archive_into(repo, r, &files, dest).await?;
        placed.extend(files.iter().map(|f| dest.join(f)));
    }
    Ok(placed)
}

pub fn remove_overlay(placed: &[PathBuf], namespace: &[String], dest: &Path) {
    for f in placed {
        let _ = std::fs::remove_file(f);
    }
    for d in namespace {
        let dir = dest.join(d.trim_end_matches('/'));
        let _ = remove_empty_dirs(&dir);
    }
}

fn remove_empty_dirs(dir: &Path) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            remove_empty_dirs(&p)?;
        }
    }
    if std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(())
}

/// The commit L1 and L2 judge is the commit that lands: nothing a check
/// command runs may move HEAD, stage anything, or leave a tracked file
/// modified, whatever it reports on exit. `before` is HEAD as `l1_l2`
/// found it, recorded before the overlay ever touches the tree; a plain
/// untracked leftover does not count (`git::dirty_tracked_paths`), only
/// HEAD itself and what git already tracks. The one legitimate mutator is
/// `try_known_fix`: it commits outside this function and calls `l1_l2`
/// again, so its own commit is `before` for that second call and must
/// still pass this row.
async fn candidate_unchanged(wt: &Path, before: &str) -> Result<CheckResult> {
    let after = crate::git::head(wt).await?;
    let dirty = crate::git::dirty_tracked_paths(wt).await?;
    let moved = after != before;
    let detail = if !moved && dirty.is_empty() {
        String::new()
    } else if moved && !dirty.is_empty() {
        format!(
            "the checks committed {after} over the verified {before} and left uncommitted changes to {}",
            dirty.join(", ")
        )
    } else if moved {
        format!("the checks committed {after} over the verified {before}")
    } else {
        format!(
            "the checks left uncommitted changes to {} on the verified {before}",
            dirty.join(", ")
        )
    };
    Ok(l0(
        Rule::CandidateUnchanged,
        !moved && dirty.is_empty(),
        detail,
    ))
}

/// L1 then L2 on the tree as it stands: the namespace overlaid from the
/// trusted refs, the repository's checks, the claim rule against the
/// envelope when there is one, the task's own commands, then the overlay
/// removed so the next attempt starts blind. Shared by the verify after a
/// directive and the verify after an operation that changed the tree.
async fn l1_l2(
    s: &Subject<'_>,
    envelope: Option<&Envelope>,
    checks: &mut Vec<CheckResult>,
) -> Result<()> {
    let candidate_before = crate::git::head(s.worktree).await?;
    let placed = overlay(s.repo, s.overlay_refs, &s.cfg.namespace, s.worktree).await?;
    if !placed.is_empty() {
        s.report.emit(
            s.task_id,
            Event::Note {
                text: &format!(
                    "overlay  {} verification file(s) from {}",
                    placed.len(),
                    overlay_label(s.overlay_refs)
                ),
            },
        );
    }
    let timeout = Duration::from_secs(s.cfg.check_timeout_secs);
    let facts = s.facts();
    let mut names: Vec<&String> = s.cfg.checks.keys().collect();
    names.sort_by_key(|n| (n.as_str() != "setup", n.as_str()));
    for name in names {
        let argv = &s.cfg.checks[name];
        let r = run_one("L1", name, argv, s.worktree, s.sandbox, timeout, &facts).await;
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
        checks.push(r);
        if gate_failed {
            break;
        }
    }
    if let Some(e) = envelope {
        for claimed in e.checks_run.iter().filter(|c| c.passed) {
            let Some(ours) = checks
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
                checks.push(r);
            }
        }
    }
    let l1_ok = checks.iter().filter(|c| c.level == "L1").all(|c| c.ok);
    if l1_ok {
        for (i, cmd) in s.task_checks.iter().enumerate() {
            let name = format!("task-check-{}", i + 1);
            let argv = vec!["bash".to_string(), "-c".to_string(), cmd.clone()];
            let mut r = run_one("L2", &name, &argv, s.worktree, s.sandbox, timeout, &facts).await;
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
            checks.push(r);
        }
    }
    remove_overlay(&placed, &s.cfg.namespace, s.worktree);
    let candidate_row = candidate_unchanged(s.worktree, &candidate_before).await?;
    emit_check(s.report, s.task_id, &candidate_row);
    checks.push(candidate_row);
    Ok(())
}

/// Whether every check `checks` says failed is one `[checks.fixable]`
/// names, and if so, run its fix command: a deterministic step before any
/// agent repair. `checks` is L1/L2 exactly as `l1_l2` left it, with every
/// L0 row already true (the caller only reaches this once L0 has passed).
/// `setup` never qualifies even if a repository's `[checks.fixable]` names
/// it: a failed `setup` means the tree does not build, which stops every
/// other check from even running, and no formatter or linter fixes that.
/// `None` when nothing failed, or when a failure is not one of these
/// commands' business; `Some` once the commands have run and, if they
/// changed anything, been committed as Forge.
async fn try_known_fix(s: &Subject<'_>, checks: &[CheckResult]) -> Result<Option<KnownFix>> {
    if checks.iter().any(|c| !c.ok && c.level != "L1") {
        return Ok(None);
    }
    let mut failing: Vec<&str> = checks
        .iter()
        .filter(|c| !c.ok && c.level == "L1")
        .map(|c| c.name.as_str())
        .collect();
    if failing.is_empty() {
        return Ok(None);
    }
    failing.sort();
    failing.dedup();
    if failing
        .iter()
        .any(|n| *n == "setup" || !s.cfg.fixable.contains_key(*n))
    {
        return Ok(None);
    }
    let before = crate::git::head(s.worktree).await?;
    let timeout = Duration::from_secs(s.cfg.check_timeout_secs);
    let facts = s.facts();
    let mut ok = true;
    for name in &failing {
        let argv = &s.cfg.fixable[*name];
        let r = run_one("fix", name, argv, s.worktree, s.sandbox, timeout, &facts).await;
        ok &= r.ok;
        s.report.emit(
            s.task_id,
            Event::Note {
                text: &format!(
                    "fix      {name} ({})",
                    if r.ok { "ok" } else { "command failed" }
                ),
            },
        );
    }
    let names: Vec<String> = failing.iter().map(|n| n.to_string()).collect();
    let message = format!("fix: {}", names.join(", "));
    let commit = crate::git::commit_all(s.worktree, &message).await?;
    let diff_stat = match &commit {
        Some(sha) => crate::git::diff_shortstat(s.worktree, &before, sha)
            .await
            .unwrap_or_default(),
        None => String::new(),
    };
    s.report.emit(
        s.task_id,
        Event::Note {
            text: &match &commit {
                Some(sha) => format!("fix      committed {} as {}", names.join(", "), &sha[..8]),
                None => format!(
                    "fix      {} left nothing to commit; the checks will fail again",
                    names.join(", ")
                ),
            },
        },
    );
    Ok(Some(KnownFix {
        checks: names,
        ok: ok && commit.is_some(),
        commit,
        diff_stat,
    }))
}

/// The L0 rows about where a change landed: protected paths, the
/// directive's write scope, the verification namespace.
/// `changed` is the branch's whole change, for the rules that guard the
/// product; `changed_now` is this step's, for the directive's own write
/// scope: a scoped step after an unscoped one is judged on what it did.
fn scope_rows(
    s: &Subject<'_>,
    changed: &[String],
    changed_now: &[String],
    dirty: &[String],
) -> Vec<CheckResult> {
    let mut rows = Vec::new();
    if !s.cfg.protected.is_empty() && !s.allow_protected {
        let hit: Vec<&str> = changed
            .iter()
            .chain(dirty.iter())
            .map(String::as_str)
            .filter(|p| is_protected(&s.cfg.protected, p))
            .collect();
        rows.push(l0(
            Rule::ProtectedPaths,
            hit.is_empty(),
            format!(
                "protected paths changed: {}. They guard the product; only a task created with --allow-protected may change them.",
                hit.join(", ")
            ),
        ));
    }
    if !s.paths.is_empty() {
        let outside: Vec<&str> = changed_now
            .iter()
            .chain(dirty.iter())
            .map(String::as_str)
            .filter(|p| !crate::config::in_scope(s.paths, p))
            .collect();
        rows.push(l0(
            Rule::PathsInScope,
            outside.is_empty(),
            format!(
                "this directive may only change {}; it changed: {}",
                s.paths.join(", "),
                outside.join(", ")
            ),
        ));
    }
    if !s.cfg.namespace.is_empty() {
        let hit: Vec<&str> = changed
            .iter()
            .chain(dirty.iter())
            .map(String::as_str)
            .filter(|p| in_namespace(&s.cfg.namespace, p))
            .collect();
        rows.push(l0(
            Rule::NamespaceUntouched,
            hit.is_empty(),
            format!("files created under the verification namespace: {}. That namespace is reserved for the tests that judge this work.", hit.join(", ")),
        ));
    }
    rows
}

/// The verdict on what an operation changed. There is no agent and no
/// envelope, so L0 is the tree alone: clean, protected paths untouched,
/// nothing under the verification namespace. Then L1 and L2 exactly as
/// after a directive. The operation's commit is already on the branch;
/// `start_sha` is the commit before it.
pub async fn verify_operation(s: Subject<'_>) -> Result<Verdict> {
    let changed = crate::git::changed_paths(s.worktree, s.start_sha).await?;
    let dirty = crate::git::dirty_paths(s.worktree).await?;
    let facts = GitFacts {
        commits: crate::git::count_commits(s.worktree, s.start_sha).await?,
        changed_now: changed.clone(),
        changed,
        dirty,
    };
    let (changed, dirty) = (&facts.changed, &facts.dirty);
    let mut v = Verdict::open(&facts);
    v.checks.push(l0(
        Rule::CleanTree,
        dirty.is_empty(),
        format!("left uncommitted by the operation: {}", dirty.join(", ")),
    ));
    v.checks
        .push(no_stray_files(s.worktree, s.start_sha).await?);
    v.checks.extend(scope_rows(&s, changed, changed, dirty));
    emit_rows(s.report, s.task_id, &v.checks);
    if v.checks.iter().all(|c| c.ok) {
        l1_l2(&s, None, &mut v.checks).await?;
    }
    v.settle(None, None, true);
    Ok(v)
}

/// Landing: the branch with the base merged in, run through every check
/// with every hidden suite overlaid. No agent, so no result contract; the
/// tree must be clean and the checks green. The scope rules run again too
/// (protected paths, `forge.toml`, the write scope, the verification
/// namespace), against the merged tree rather than the branch alone: a
/// merge can carry a change past L0 that no single directive committed by
/// itself.
pub async fn verify_integration(s: &Subject<'_>) -> Result<Verdict> {
    let changed = crate::git::changed_paths(s.worktree, s.base_sha).await?;
    let dirty = crate::git::dirty_paths(s.worktree).await?;
    let facts = GitFacts {
        commits: crate::git::count_commits(s.worktree, s.base_sha).await?,
        changed_now: changed.clone(),
        changed,
        dirty,
    };
    let (changed, dirty) = (&facts.changed, &facts.dirty);
    let mut v = Verdict::open(&facts);
    v.checks.push(l0(
        Rule::CleanTree,
        dirty.is_empty(),
        format!("uncommitted after the merge: {}", dirty.join(", ")),
    ));
    let touched = changed.iter().any(|p| p == s.cfg.config_path.as_str());
    v.checks.push(l0(
        Rule::ConfigUntouched,
        !touched,
        format!("the merged tree modifies {}", s.cfg.config_path),
    ));
    v.checks.extend(scope_rows(s, changed, changed, dirty));
    emit_rows(s.report, s.task_id, &v.checks);
    if v.checks.iter().all(|c| c.ok) {
        l1_l2(s, None, &mut v.checks).await?;
    }
    v.settle(None, None, true);
    Ok(v)
}

/// A directive's verdict: the shared L0, the contract's own rows, then
/// the checks the contract runs, then the decision. One path for every
/// contract; what differs is under the match.
///
/// - code: the write scope rows, then L1 and L2 with the hidden suites
///   overlaid.
/// - tests: only the namespace changed, the interface is described, and
///   the new tests fail on the base (red-on-base) in a scratch copy.
/// - review: no writes, and a demotion stands only with something run.
/// - plan: untouched, and a plan that is substantive and names real paths.
pub async fn verify_directive(
    contract: Contract,
    s: &Subject<'_>,
    agent: &Outcome,
) -> Result<Verdict> {
    if s.sandbox
        .is_some_and(|e| !e.guarantees(s.worktree).checks_under_kernel_control)
    {
        let mut verdict = Verdict::open(&GitFacts::default());
        verdict.state = AttemptState::Unverified;
        verdict.reason = "remote executor: the kernel could not run the checks itself".into();
        return Ok(verdict);
    }
    let agent_reason = crate::directive::agent_failure(agent);
    let common = common_l0(s, agent).await?;
    let mut v = Verdict::open(&common.facts);
    let mut question: Option<(Kind, String)> = None;
    if agent_reason.is_none() {
        let facts = &common.facts;
        v.checks = common
            .rows
            .into_iter()
            .filter(|r| match Rule::parse(&r.name) {
                Some(rule) => contract_keeps(contract, rule),
                None => true,
            })
            .collect();
        question = common.question;
        match contract {
            Contract::Code => {
                v.checks.extend(scope_rows(
                    s,
                    &facts.changed,
                    &facts.changed_now,
                    &facts.dirty,
                ));
                emit_rows(s.report, s.task_id, &v.checks);
                // A question does not excuse the checks: when the tree is
                // clean and the attempt committed (every L0 row above
                // passed), L1 runs exactly as it would for a succeeded
                // attempt, so the verdict says whether the committed work
                // is any good, not only that a question was asked. The
                // attempt still ends needs_input; `decide` gives the
                // question priority over these rows.
                if v.checks.iter().all(|c| c.ok) {
                    l1_l2(s, common.envelope.as_ref(), &mut v.checks).await?;
                    // Deterministic repair before any agent gets involved: a
                    // failure only in checks `[checks.fixable]` names is
                    // fixed, committed, and the checks run once more.
                    // Skipped when a question is pending: `decide` gives it
                    // priority over the checks regardless, so a fix here
                    // would be wasted work.
                    if question.is_none()
                        && let Some(fix) = try_known_fix(s, &v.checks).await?
                    {
                        v.checks.retain(|c| {
                            c.level != "L1"
                                && c.level != "L2"
                                && c.name != Rule::CandidateUnchanged.name()
                        });
                        l1_l2(s, common.envelope.as_ref(), &mut v.checks).await?;
                        v.known_fix = Some(fix);
                    }
                }
            }
            Contract::Tests => {
                let outside: Vec<&str> = facts
                    .changed
                    .iter()
                    .chain(facts.dirty.iter())
                    .map(String::as_str)
                    .filter(|p| !in_namespace(&s.cfg.namespace, p))
                    .collect();
                v.checks.push(l0(
                    Rule::NamespaceOnly,
                    outside.is_empty(),
                    format!(
                        "the tests step may only change {}; it changed: {}",
                        s.cfg.namespace.join(", "),
                        outside.join(", ")
                    ),
                ));
                let has_summary = common
                    .envelope
                    .as_ref()
                    .is_some_and(|e| e.summary.trim().len() >= 40);
                v.checks.push(l0(
                    Rule::InterfaceDescribed,
                    has_summary,
                    "the summary must describe the interface the tests expect; it is all the implementer will see".into(),
                ));
                emit_rows(s.report, s.task_id, &v.checks);
                if v.checks.iter().all(|c| c.ok) && question.is_none() {
                    red_on_base(s, &mut v.checks).await?;
                }
            }
            Contract::Review => {
                let added = crate::git::changed_paths(s.worktree, s.start_sha).await?;
                v.checks.push(l0(
                    Rule::NoWrites,
                    added.is_empty() && facts.dirty.is_empty(),
                    format!(
                        "the reviewer changed the branch: {}",
                        added
                            .iter()
                            .chain(facts.dirty.iter())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
                v.checks.push(CheckResult {
                    level: Rule::ExecutedSomething.level().into(),
                    name: Rule::ExecutedSomething.name().into(),
                    ok: agent.tool_calls > 0,
                    tail: if agent.tool_calls > 0 { String::new() } else { "the reviewer ran no tool; a review that reads without running is an opinion, so any demotion is ignored".into() },
                    ..Default::default()
                });
                emit_rows(s.report, s.task_id, &v.checks);
                // A demotion stands only with something executed.
                if let Some((Kind::Review, text)) = &question
                    && agent.tool_calls == 0
                {
                    s.report.emit(
                        s.task_id,
                        Event::Note {
                            text: &format!(
                                "review   demotion ignored (no executed evidence): {text}"
                            ),
                        },
                    );
                    question = None;
                }
            }
            Contract::Plan => {
                let added = crate::git::changed_paths(s.worktree, s.start_sha).await?;
                v.checks.push(l0(
                    Rule::Untouched,
                    added.is_empty() && facts.dirty.is_empty(),
                    format!(
                        "the investigator changed the branch: {}",
                        added
                            .iter()
                            .chain(facts.dirty.iter())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
                if question.is_none() && s.plan_rows {
                    v.checks.extend(plan_rows(s, common.envelope.as_ref()));
                }
                emit_rows(s.report, s.task_id, &v.checks);
            }
        }
        v.envelope = common.envelope;
    }
    v.settle(
        agent_reason.as_deref(),
        question.as_ref().map(|(k, q)| (*k, q.as_str())),
        contract.verifies_work(),
    );
    Ok(v)
}

/// Which of the shared L0 rows a contract is held to. A read-only
/// contract commits nothing and reports no changes, so those two rows
/// do not apply; a plan is judged on its result and its restraint alone.
fn contract_keeps(contract: Contract, rule: Rule) -> bool {
    match contract {
        Contract::Code | Contract::Tests => true,
        Contract::Review => !matches!(rule, Rule::HasCommits | Rule::ChangesFromGit),
        Contract::Plan => matches!(rule, Rule::ResultStructured | Rule::CleanTree),
    }
}

/// The plan's own rows: substantive, and naming only paths that exist or
/// new files in directories that do. Plans create files; they do not
/// invent directories.
fn plan_rows(s: &Subject<'_>, envelope: Option<&Envelope>) -> Vec<CheckResult> {
    let plan = envelope
        .map(|e| e.summary.trim().to_string())
        .unwrap_or_default();
    let missing: Vec<String> = plan_paths(&plan, &|d| s.worktree.join(d).is_dir())
        .into_iter()
        .filter(|p| {
            let path = s.worktree.join(p);
            !path.exists() && !path.parent().is_some_and(|d| d.is_dir())
        })
        .collect();
    vec![
        l0(
            Rule::PlanSubstantive,
            plan.chars().count() >= 120,
            format!(
                "a plan of {} characters is not a plan; name the files, the changes, and the test",
                plan.chars().count()
            ),
        ),
        l0(
            Rule::PlanNamesRealPaths,
            missing.is_empty(),
            format!(
                "the plan names paths that do not exist in the tree, in directories that do not exist either: {}",
                missing.join(", ")
            ),
        ),
    ]
}

/// Red on base: the base tree plus the new tests, `setup` then `test`,
/// in the scratch directory. The row passes when the tests FAIL on base.
async fn red_on_base(s: &Subject<'_>, checks: &mut Vec<CheckResult>) -> Result<()> {
    let scratch = s
        .scratch
        .ok_or_else(|| anyhow::anyhow!("the tests contract needs a scratch directory"))?;
    let _ = std::fs::remove_dir_all(scratch);
    crate::git::archive_all(s.worktree, s.base_sha, scratch).await?;
    let files = crate::git::ls_tree(s.worktree, "HEAD", &s.cfg.namespace).await?;
    crate::git::archive_into(s.worktree, "HEAD", &files, scratch).await?;
    let timeout = Duration::from_secs(s.cfg.check_timeout_secs);
    let facts = s.facts();
    let mut setup_ok = true;
    if let Some(argv) = s.cfg.checks.get("setup") {
        let r = run_one("L1", "setup", argv, scratch, s.sandbox, timeout, &facts).await;
        emit_check(s.report, s.task_id, &r);
        setup_ok = r.ok;
        checks.push(r);
    }
    if setup_ok {
        let argv = s.cfg.checks.get("test").cloned().unwrap_or_default();
        let mut r = run_one(
            Rule::RedOnBase.level(),
            Rule::RedOnBase.name(),
            &argv,
            scratch,
            s.sandbox,
            timeout,
            &facts,
        )
        .await;
        let failed_on_base = !r.ok && !r.timed_out;
        r.ok = failed_on_base;
        if !failed_on_base {
            r.tail = format!(
                "the new tests {} on the base commit, so they do not specify the task\n{}",
                if r.timed_out { "timed out" } else { "pass" },
                r.tail
            );
        }
        emit_check(s.report, s.task_id, &r);
        checks.push(r);
    }
    let _ = std::fs::remove_dir_all(scratch);
    Ok(())
}

/// Path-like tokens in a plan: anything with a source extension, or a
/// slash-separated token whose first segment is a directory of the tree
/// (`is_dir` says), stripped of the punctuation prose wraps it in. The
/// directory test keeps prose such as `$/LANDED` or `none/dash` out.
pub fn plan_paths(text: &str, is_dir: &dyn Fn(&str) -> bool) -> Vec<String> {
    const EXT: &[&str] = &[
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".py", ".sh", ".go", ".toml", ".md", ".json",
        ".yml", ".yaml", ".html", ".css", ".sql", ".txt",
    ];
    let mut out = Vec::new();
    for raw in
        text.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '(' || c == ')')
    {
        let tok = raw.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '\'' | '"' | ':' | '.' | '*' | '[' | ']' | '<' | '>'
            )
        });
        let tok = tok.split(':').next().unwrap_or(tok); // path:line
        if tok.is_empty()
            || tok.starts_with("http")
            || tok.starts_with('-')
            || tok.ends_with('/')
            || raw.contains("..")
        {
            continue;
        }
        let plain = tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '@'));
        if !plain {
            continue;
        }
        let looks = EXT.iter().any(|e| tok.ends_with(e))
            || (tok.contains('/')
                && !tok.starts_with('/')
                && is_dir(tok.split('/').next().unwrap_or("")));
        if looks && !out.contains(&tok.to_string()) {
            out.push(tok.to_string());
        }
    }
    out
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
    fn namespace_membership() {
        let ns = vec!["tests/acceptance/".to_string()];
        assert!(in_namespace(&ns, "tests/acceptance/a.sh"));
        assert!(!in_namespace(&ns, "tests/acceptance.sh"));
        assert!(!in_namespace(&ns, "src/a.ts"));
    }

    /// A tempdir with a base commit and a second commit that adds
    /// `real.txt`, as an attempt's own work. Returns the directory and the
    /// base sha, the attempt's `start_sha`.
    async fn commit_fixture() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "a@a.com"]);
        run(&["config", "user.name", "a"]);
        std::fs::write(dir.path().join("base.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "--quiet", "-m", "base"]);
        let base = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        std::fs::write(dir.path().join("real.txt"), "two\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "--quiet", "-m", "attempt"]);
        (dir, base)
    }

    #[tokio::test]
    async fn no_stray_files_rejects_matching_additions() {
        let (dir, base) = commit_fixture().await;
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        let names = [
            "file.bak",
            "file.backup",
            "file.orig",
            "file.rej",
            "file~",
            "temp_file",
            "tmp_file",
            "scratch_file",
        ];
        for name in names {
            std::fs::write(dir.path().join("nested").join(name), name).unwrap();
        }
        crate::git::commit_all(dir.path(), "add strays")
            .await
            .unwrap();
        let row = no_stray_files(dir.path(), &base).await.unwrap();
        assert!(!row.ok);
        assert_eq!(row.name, "no-stray-files");
        assert_eq!(row.level, "L0");
        for name in names {
            assert!(row.tail.contains(&format!("nested/{name}")));
        }
        assert!(row.tail.contains("Delete them"));
    }

    #[tokio::test]
    async fn no_stray_files_rejects_non_ascii_backup_names() {
        let (dir, base) = commit_fixture().await;
        std::fs::write(dir.path().join("résumé.bak"), "cv").unwrap();
        crate::git::commit_all(dir.path(), "add non-ascii backup")
            .await
            .unwrap();
        let row = no_stray_files(dir.path(), &base).await.unwrap();
        assert!(!row.ok);
        assert!(row.tail.contains("résumé.bak"));
    }

    #[tokio::test]
    async fn no_stray_files_accepts_non_matching_additions_and_existing_backups() {
        let (dir, _) = commit_fixture().await;
        std::fs::write(dir.path().join("existing.bak"), "old").unwrap();
        crate::git::commit_all(dir.path(), "existing backup")
            .await
            .unwrap();
        let start = crate::git::head(dir.path()).await.unwrap();
        std::fs::write(dir.path().join("existing.bak"), "modified").unwrap();
        std::fs::create_dir(dir.path().join("scratch_directory")).unwrap();
        for name in [
            "backup.rs",
            "file.bak.rs",
            "temporary.txt",
            "scratch_directory/real.rs",
        ] {
            std::fs::write(dir.path().join(name), "real work").unwrap();
        }
        crate::git::commit_all(dir.path(), "normal changes")
            .await
            .unwrap();
        assert!(no_stray_files(dir.path(), &start).await.unwrap().ok);
    }

    #[tokio::test]
    async fn no_stray_files_accepts_a_file_added_then_deleted_in_the_attempt() {
        let (dir, base) = commit_fixture().await;
        let path = dir.path().join("scratch_work");
        std::fs::write(&path, "scratch").unwrap();
        crate::git::commit_all(dir.path(), "add scratch")
            .await
            .unwrap();
        std::fs::remove_file(path).unwrap();
        crate::git::commit_all(dir.path(), "delete scratch")
            .await
            .unwrap();
        assert!(no_stray_files(dir.path(), &base).await.unwrap().ok);
    }

    fn test_cfg() -> Config {
        Config {
            execution: Default::default(),
            checks: std::collections::BTreeMap::new(),
            fixable: std::collections::BTreeMap::new(),
            base_branch: "main".into(),
            push_remote: None,
            check_timeout_secs: 60,
            protected: vec![],
            namespace: vec![],
            egress: vec![],
            environment_deny: vec![],
            config_path: "forge.toml".into(),
        }
    }

    /// A structured result naming a file the attempt never touched instead
    /// of the one it actually committed: the retired `changes-match-git`
    /// rule would have failed this, exactly the false-claim-about-the-report
    /// shape dev.home's qwen3-coder:30b hit on task 324. Every provider now
    /// has its `changes[]` derived from git, so the wrong list is simply
    /// replaced, never a reason to fail.
    fn wrong_changes_outcome() -> Outcome {
        Outcome {
            structured: Some(
                r#"{"schema_version":1,"summary":"did it","needs_input":null,"changes":[{"path":"wrong.txt","kind":"added"}],"checks_run":[],"claims":[]}"#
                    .into(),
            ),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn changes_from_git_replaces_a_wrong_changes_list_for_every_provider() {
        let (dir, base) = commit_fixture().await;
        let cfg = test_cfg();
        let report = Reporter::new(false, None);
        let outcome = wrong_changes_outcome();
        let s = Subject {
            task_id: 1,
            repo: dir.path(),
            worktree: dir.path(),
            base_sha: &base,
            start_sha: &base,
            branch: "forge/1",
            cfg: &cfg,
            task_checks: &[],
            paths: &[],
            allow_protected: false,
            overlay_refs: &[],
            pending_main: None,
            sandbox: None,
            report: &report,
            scratch: None,
            plan_rows: true,
        };
        let common = common_l0(&s, &outcome).await.unwrap();
        let note = common
            .rows
            .iter()
            .find(|r| r.name == "changes-from-git")
            .expect("a changes-from-git note row");
        assert!(note.ok);
        assert!(common.rows.iter().all(|r| r.name != "changes-match-git"));
        let changes = common.envelope.unwrap().changes;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "real.txt");
        assert_eq!(changes[0].kind, "added");
    }

    #[tokio::test]
    async fn verify_integration_reruns_the_scope_rules_over_the_merged_tree() {
        // The base has no protected paths; the "merge" (a plain commit
        // stands in for one here, since only the diff against `base_sha`
        // matters to the rule) carries a change to one anyway — the shape
        // a real merge could produce even though no single directive
        // committed it by itself.
        let (dir, base) = commit_fixture().await;
        std::fs::write(dir.path().join("secrets.txt"), "leak\n").unwrap();
        crate::git::commit_all(dir.path(), "merge carrying a protected change")
            .await
            .unwrap();
        let mut cfg = test_cfg();
        cfg.protected = vec!["secrets.txt".to_string()];
        let report = Reporter::new(false, None);
        let s = Subject {
            task_id: 1,
            repo: dir.path(),
            worktree: dir.path(),
            base_sha: &base,
            start_sha: &base,
            branch: "forge/1",
            cfg: &cfg,
            task_checks: &[],
            paths: &[],
            allow_protected: false,
            overlay_refs: &[],
            pending_main: None,
            sandbox: None,
            report: &report,
            scratch: None,
            plan_rows: true,
        };
        let v = verify_integration(&s).await.unwrap();
        assert_eq!(
            v.checks
                .iter()
                .find(|c| c.name == "protected-paths")
                .map(|c| c.ok),
            Some(false),
            "{:?}",
            v.checks
        );
        assert_eq!(v.state, AttemptState::ChecksFailed);
        assert_eq!(v.reason, "L0 failed: protected-paths");
    }

    #[tokio::test]
    async fn l1_l2_fails_candidate_unchanged_when_a_check_commits() {
        let (dir, base) = commit_fixture().await;
        let mut cfg = test_cfg();
        cfg.checks.insert(
            "tamper".into(),
            vec![
                "bash".into(),
                "-c".into(),
                "echo tampered >> forge.toml; git -c user.email=a@a.com -c user.name=a commit --quiet -am tamper".into(),
            ],
        );
        std::fs::write(dir.path().join("forge.toml"), "[checks]\n").unwrap();
        crate::git::commit_all(dir.path(), "add forge.toml")
            .await
            .unwrap();
        let before = crate::git::head(dir.path()).await.unwrap();
        let report = Reporter::new(false, None);
        let s = Subject {
            task_id: 1,
            repo: dir.path(),
            worktree: dir.path(),
            base_sha: &base,
            start_sha: &base,
            branch: "forge/1",
            cfg: &cfg,
            task_checks: &[],
            paths: &[],
            allow_protected: false,
            overlay_refs: &[],
            pending_main: None,
            sandbox: None,
            report: &report,
            scratch: None,
            plan_rows: true,
        };
        let mut checks = Vec::new();
        l1_l2(&s, None, &mut checks).await.unwrap();
        let row = checks
            .iter()
            .find(|c| c.name == "candidate-unchanged")
            .expect("a candidate-unchanged row");
        assert!(!row.ok, "{row:?}");
        assert!(row.tail.contains(&before), "{}", row.tail);
        let after = crate::git::head(dir.path()).await.unwrap();
        assert!(row.tail.contains(&after), "{}", row.tail);
    }

    fn fixable_cfg(fixable: &[(&str, &[&str])]) -> Config {
        let mut cfg = test_cfg();
        cfg.checks.insert("fmt".into(), vec!["true".into()]);
        cfg.checks.insert("setup".into(), vec!["true".into()]);
        for (name, argv) in fixable {
            cfg.fixable.insert(
                name.to_string(),
                argv.iter().map(|s| s.to_string()).collect(),
            );
        }
        cfg
    }

    #[tokio::test]
    async fn try_known_fix_runs_the_command_commits_and_reports_the_diff_stat() {
        let (dir, _base) = commit_fixture().await;
        std::fs::write(dir.path().join("fmt.txt"), "BAD\n").unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["add", "."])
            .status()
            .unwrap();
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["commit", "--quiet", "-m", "bad fmt"])
            .status()
            .unwrap();
        let cfg = fixable_cfg(&[("fmt", &["bash", "-c", "echo GOOD > fmt.txt"])]);
        let report = Reporter::new(false, None);
        let s = Subject {
            task_id: 1,
            repo: dir.path(),
            worktree: dir.path(),
            base_sha: "",
            start_sha: "",
            branch: "forge/1",
            cfg: &cfg,
            task_checks: &[],
            paths: &[],
            allow_protected: false,
            overlay_refs: &[],
            pending_main: None,
            sandbox: None,
            report: &report,
            scratch: None,
            plan_rows: true,
        };
        let checks = vec![
            l0(Rule::CleanTree, true, String::new()),
            CheckResult {
                level: "L1".into(),
                name: "fmt".into(),
                ok: false,
                ..Default::default()
            },
        ];
        let fix = try_known_fix(&s, &checks)
            .await
            .unwrap()
            .expect("fmt is declared fixable, so a fix must run");
        assert!(fix.ok);
        assert_eq!(fix.checks, vec!["fmt".to_string()]);
        assert!(fix.commit.is_some());
        assert!(!fix.diff_stat.is_empty(), "{fix:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("fmt.txt")).unwrap(),
            "GOOD\n"
        );
    }

    #[tokio::test]
    async fn try_known_fix_declines_when_a_failing_check_is_not_fixable() {
        let (dir, _base) = commit_fixture().await;
        let cfg = fixable_cfg(&[("fmt", &["bash", "-c", "echo GOOD > fmt.txt"])]);
        let report = Reporter::new(false, None);
        let s = Subject {
            task_id: 1,
            repo: dir.path(),
            worktree: dir.path(),
            base_sha: "",
            start_sha: "",
            branch: "forge/1",
            cfg: &cfg,
            task_checks: &[],
            paths: &[],
            allow_protected: false,
            overlay_refs: &[],
            pending_main: None,
            sandbox: None,
            report: &report,
            scratch: None,
            plan_rows: true,
        };
        let checks = vec![CheckResult {
            level: "L1".into(),
            name: "clippy".into(),
            ok: false,
            ..Default::default()
        }];
        assert!(try_known_fix(&s, &checks).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn try_known_fix_declines_a_failed_setup_even_if_named_fixable() {
        let (dir, _base) = commit_fixture().await;
        let cfg = fixable_cfg(&[("setup", &["bash", "-c", "true"])]);
        let report = Reporter::new(false, None);
        let s = Subject {
            task_id: 1,
            repo: dir.path(),
            worktree: dir.path(),
            base_sha: "",
            start_sha: "",
            branch: "forge/1",
            cfg: &cfg,
            task_checks: &[],
            paths: &[],
            allow_protected: false,
            overlay_refs: &[],
            pending_main: None,
            sandbox: None,
            report: &report,
            scratch: None,
            plan_rows: true,
        };
        let checks = vec![CheckResult {
            level: "L1".into(),
            name: "setup".into(),
            ok: false,
            ..Default::default()
        }];
        assert!(try_known_fix(&s, &checks).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn try_known_fix_declines_when_an_l0_row_also_failed() {
        let (dir, _base) = commit_fixture().await;
        let cfg = fixable_cfg(&[("fmt", &["bash", "-c", "echo GOOD > fmt.txt"])]);
        let report = Reporter::new(false, None);
        let s = Subject {
            task_id: 1,
            repo: dir.path(),
            worktree: dir.path(),
            base_sha: "",
            start_sha: "",
            branch: "forge/1",
            cfg: &cfg,
            task_checks: &[],
            paths: &[],
            allow_protected: false,
            overlay_refs: &[],
            pending_main: None,
            sandbox: None,
            report: &report,
            scratch: None,
            plan_rows: true,
        };
        let checks = vec![
            l0(Rule::CleanTree, false, "dirty".into()),
            CheckResult {
                level: "L1".into(),
                name: "fmt".into(),
                ok: false,
                ..Default::default()
            },
        ];
        assert!(try_known_fix(&s, &checks).await.unwrap().is_none());
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
    fn plan_paths_finds_files_and_ignores_prose() {
        let plan = "Change `src/cli.rs` (the run_doctor fn) and src/doctor.rs:112; add tests/e2e.rs::doctor_json. \
                    See https://example.com/x and docs/ACTIONS.md. Not paths: a/b/.., /abs/path, foo, $/LANDED, \
                    OK/$/OK, none/dash, wf[\"$/LANDED. A new file in a real dir: src/new_mod.rs and web/x.";
        let is_dir = |d: &str| matches!(d, "src" | "tests" | "docs" | "web");
        assert_eq!(
            plan_paths(plan, &is_dir),
            vec![
                "src/cli.rs",
                "src/doctor.rs",
                "tests/e2e.rs",
                "docs/ACTIONS.md",
                "src/new_mod.rs",
                "web/x"
            ]
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
            known_fix: None,
        };
        let fb = feedback(&v, &Outcome::default(), 30);
        assert!(fb.contains("failing tests: TestA"), "{fb}");
        assert!(fb.contains("- L1 test (exit 1)"), "{fb}");
    }
}

/// The first failed L1 check whose reported file locations all lie inside
/// the verification namespace: the tests' fault, not the implementer's,
/// who cannot see or edit those files. The check name and its tail.
pub fn tests_fault(checks: &[CheckResult], namespace: &[String]) -> Option<(String, String)> {
    checks
        .iter()
        .filter(|c| !c.ok && c.level == "L1")
        .find_map(|c| {
            let locs = locations(&c.tail);
            let inside = !locs.is_empty()
                && locs
                    .iter()
                    .all(|l| namespace.iter().any(|n| l.starts_with(n.as_str())));
            inside.then(|| (c.name.clone(), last_lines(&c.tail, 20)))
        })
}

/// File locations mentioned in check output, in the two shapes compilers
/// and linters print: `path(line,col)` and `path:line`.
fn locations(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let start = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'.' | b'/' | b'-'))
            {
                i += 1;
            }
            if i > start
                && i < b.len()
                && matches!(b[i], b'(' | b':')
                && b.get(i + 1).is_some_and(|c| c.is_ascii_digit())
            {
                let tok = line[start..i].trim_start_matches("./");
                if tok.contains('.') && !tok.starts_with('.') {
                    out.push(tok.to_string());
                }
            }
            if i == start {
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests_fault_tests {
    use super::*;

    fn check(name: &str, ok: bool, tail: &str) -> CheckResult {
        CheckResult {
            level: "L1".into(),
            name: name.into(),
            ok,
            exit: Some(if ok { 0 } else { 2 }),
            ms: 1,
            timed_out: false,
            tail: tail.into(),
            failing_tests: vec![],
            stdout: String::new(),
        }
    }

    #[test]
    fn locations_come_in_compiler_and_grep_shapes() {
        let t = "tests/acceptance/p.test.ts(27,12): error TS2532: x\n./src/a.ts:3:1 - warning\nerror TS1234 plain\n";
        assert_eq!(locations(t), vec!["tests/acceptance/p.test.ts", "src/a.ts"]);
    }

    #[test]
    fn a_failure_only_inside_the_namespace_is_the_tests_fault() {
        let ns = vec!["tests/acceptance/".to_string()];
        let only = check(
            "typecheck",
            false,
            "tests/acceptance/p.test.ts(27,12): error TS2532\ntests/acceptance/p.test.ts(28,12): error TS2532",
        );
        assert_eq!(
            tests_fault(&[check("lint", true, ""), only], &ns).map(|f| f.0),
            Some("typecheck".into())
        );
        let mixed = check(
            "typecheck",
            false,
            "tests/acceptance/p.test.ts(27,12): error\nsrc/sim/data.ts(4,1): error",
        );
        assert!(
            tests_fault(&[mixed], &ns).is_none(),
            "an error in the implementer's own files is theirs"
        );
        let none = check("test", false, "FAIL 3 tests\nexpected 42 got 41");
        assert!(
            tests_fault(&[none], &ns).is_none(),
            "no locations, no attribution"
        );
        let passing = check("typecheck", true, "tests/acceptance/p.test.ts(1,1): note");
        assert!(tests_fault(&[passing], &ns).is_none());
    }
}
