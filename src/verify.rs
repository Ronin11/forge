//! Verification: claims are verified, not believed. Three levels, each run
//! by Forge after the agent has exited, each a row in the verdict.
//!
//! - L0: is the result consistent with git? A structured result exists,
//!   the tree is clean, there is at least one commit, `forge.toml`,
//!   protected paths, and the verification namespace are untouched, the
//!   reported `changes[]` match what git saw, every claim has evidence.
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
use crate::envelope::{self, Envelope, Kind};
use crate::report::{Event, Reporter};
use crate::sandbox::Sandbox;
use crate::store::AttemptState;
use anyhow::Result;
use std::collections::BTreeSet;
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

/// Every row the kernel writes about an attempt's own conduct, by name.
/// The names are what the trace, the events, the audit and the tests
/// carry; adding a rule here makes the audit's table refuse to compile
/// until it has a line for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    ResultStructured,
    SuiteNamesAHiddenTest,
    CleanTree,
    ConfigUntouched,
    HasCommits,
    ChangesMatchGit,
    ClaimsHaveEvidence,
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
    pub const ALL: [Rule; 21] = [
        Rule::ResultStructured,
        Rule::SuiteNamesAHiddenTest,
        Rule::CleanTree,
        Rule::ConfigUntouched,
        Rule::HasCommits,
        Rule::ChangesMatchGit,
        Rule::ClaimsHaveEvidence,
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
            Rule::ConfigUntouched => "forge.toml-untouched",
            Rule::HasCommits => "has-commits",
            Rule::ChangesMatchGit => "changes-match-git",
            Rule::ClaimsHaveEvidence => "claims-have-evidence",
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
    /// so it is L1; `executed-something` is a note that never decides.
    pub fn level(self) -> &'static str {
        match self {
            Rule::RedOnBase => "L1",
            Rule::ExecutedSomething => "note",
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

/// What every step's L0 shares: git facts, the envelope, the rows that do
/// not depend on the step. Returns the rows, the envelope, and a question.
#[allow(clippy::too_many_arguments)]
async fn common_l0(
    worktree: &Path,
    base_sha: &str,
    start_sha: &str,
    pending_main: Option<&str>,
    cfg: &Config,
    agent: &Outcome,
    report: &Reporter,
    task_id: i64,
) -> Result<(
    Vec<CheckResult>,
    Option<Envelope>,
    Option<(Kind, String)>,
    i64,
    Vec<String>,
    Vec<String>,
    Vec<String>,
)> {
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
    let env = match &parsed {
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
    if let Some(e) = &env {
        let reported: BTreeSet<&str> = e.changes.iter().map(|c| c.path.as_str()).collect();
        let actual: BTreeSet<&str> = changed_this_attempt
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
        rows.push(l0(
            Rule::ChangesMatchGit,
            unreported.is_empty() && phantom.is_empty(),
            detail.trim().to_string(),
        ));
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
    Ok((
        rows,
        env,
        question,
        commits,
        changed,
        changed_this_attempt,
        dirty,
    ))
}

fn emit_rows(report: &Reporter, task_id: i64, rows: &[CheckResult]) {
    for c in rows {
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

/// The code step's verdict.
pub async fn verify(s: Subject<'_>, agent: &Outcome) -> Result<Verdict> {
    let agent_reason = agent_failure(agent);
    let mut v = Verdict {
        commits: 0,
        files_changed: 0,
        dirty: false,
        envelope: None,
        checks: Vec::new(),
        state: AttemptState::Running,
        reason: String::new(),
    };
    let mut question: Option<(Kind, String)> = None;
    let (rows, env, q, commits, changed, changed_now, dirty) = common_l0(
        s.worktree,
        s.base_sha,
        s.start_sha,
        s.pending_main,
        s.cfg,
        agent,
        s.report,
        s.task_id,
    )
    .await?;
    v.commits = commits;
    v.files_changed = changed.len() as i64;
    v.dirty = !dirty.is_empty();
    if agent_reason.is_none() {
        v.checks = rows;
        question = q;
        v.checks
            .extend(scope_rows(&s, &changed, &changed_now, &dirty));
        emit_rows(s.report, s.task_id, &v.checks);
        let l0_ok = v.checks.iter().all(|c| c.ok) && question.is_none();

        if l0_ok {
            l1_l2(&s, env.as_ref(), &mut v.checks).await?;
        }
        v.envelope = env;
    }
    let (state, reason) = decide(
        agent_reason.as_deref(),
        question.as_ref().map(|(k, q)| (*k, q.as_str())),
        &v.checks,
    );
    v.state = state;
    v.reason = reason;
    Ok(v)
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
    let mut names: Vec<&String> = s.cfg.checks.keys().collect();
    names.sort_by_key(|n| (n.as_str() != "setup", n.as_str()));
    for name in names {
        let argv = &s.cfg.checks[name];
        let r = run_one("L1", name, argv, s.worktree, s.sandbox, timeout, &[]).await;
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
            let mut r = run_one("L2", &name, &argv, s.worktree, s.sandbox, timeout, &[]).await;
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
    Ok(())
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
    let mut v = Verdict {
        commits: crate::git::count_commits(s.worktree, s.start_sha).await?,
        files_changed: 0,
        dirty: false,
        envelope: None,
        checks: Vec::new(),
        state: AttemptState::Running,
        reason: String::new(),
    };
    let changed = crate::git::changed_paths(s.worktree, s.start_sha).await?;
    let dirty = crate::git::dirty_paths(s.worktree).await?;
    v.files_changed = changed.len() as i64;
    v.dirty = !dirty.is_empty();
    v.checks.push(l0(
        Rule::CleanTree,
        dirty.is_empty(),
        format!("left uncommitted by the operation: {}", dirty.join(", ")),
    ));
    v.checks.extend(scope_rows(&s, &changed, &changed, &dirty));
    emit_rows(s.report, s.task_id, &v.checks);
    if v.checks.iter().all(|c| c.ok) {
        l1_l2(&s, None, &mut v.checks).await?;
    }
    let (state, reason) = decide(None, None, &v.checks);
    v.state = state;
    v.reason = reason;
    Ok(v)
}

/// Landing: the branch with the base merged in, run through every check
/// with every hidden suite overlaid. No agent, so no result contract; the
/// tree must be clean and the checks green.
pub async fn verify_integration(s: &Subject<'_>) -> Result<Verdict> {
    let mut v = Verdict {
        commits: crate::git::count_commits(s.worktree, s.base_sha).await?,
        files_changed: 0,
        dirty: false,
        envelope: None,
        checks: Vec::new(),
        state: AttemptState::Running,
        reason: String::new(),
    };
    let dirty = crate::git::dirty_paths(s.worktree).await?;
    v.dirty = !dirty.is_empty();
    v.checks.push(l0(
        Rule::CleanTree,
        dirty.is_empty(),
        format!("uncommitted after the merge: {}", dirty.join(", ")),
    ));
    emit_rows(s.report, s.task_id, &v.checks);
    if v.checks.iter().all(|c| c.ok) {
        l1_l2(s, None, &mut v.checks).await?;
    }
    let (state, reason) = decide(None, None, &v.checks);
    v.state = state;
    v.reason = reason;
    Ok(v)
}

pub struct TestsSubject<'a> {
    pub task_id: i64,
    /// The tests step's own clone.
    pub worktree: &'a Path,
    /// Scratch directory for the red-on-base run; created and removed here.
    pub scratch: &'a Path,
    pub base_sha: &'a str,
    pub start_sha: &'a str,
    pub cfg: &'a Config,
    pub sandbox: Option<&'a Sandbox>,
    pub report: &'a Reporter,
}

/// The tests step's verdict: L0, only the namespace changed, and the new
/// tests fail against the base commit (so they specify the task rather than
/// the status quo). Runs the repo's `setup` and `test` checks in a scratch
/// copy of base with the tests overlaid.
pub async fn verify_tests(s: TestsSubject<'_>, agent: &Outcome) -> Result<Verdict> {
    let agent_reason = agent_failure(agent);
    let mut v = Verdict {
        commits: 0,
        files_changed: 0,
        dirty: false,
        envelope: None,
        checks: Vec::new(),
        state: AttemptState::Running,
        reason: String::new(),
    };
    let mut question: Option<(Kind, String)> = None;
    let (rows, env, q, commits, changed, _changed_now, dirty) = common_l0(
        s.worktree,
        s.base_sha,
        s.start_sha,
        None,
        s.cfg,
        agent,
        s.report,
        s.task_id,
    )
    .await?;
    v.commits = commits;
    v.files_changed = changed.len() as i64;
    v.dirty = !dirty.is_empty();
    if agent_reason.is_none() {
        v.checks = rows;
        question = q;
        let outside: Vec<&str> = changed
            .iter()
            .chain(dirty.iter())
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
        let has_summary = env.as_ref().is_some_and(|e| e.summary.trim().len() >= 40);
        v.checks.push(l0(
            Rule::InterfaceDescribed,
            has_summary,
            "the summary must describe the interface the tests expect; it is all the implementer will see".into(),
        ));
        emit_rows(s.report, s.task_id, &v.checks);
        let l0_ok = v.checks.iter().all(|c| c.ok) && question.is_none();

        if l0_ok {
            // Red on base: base tree plus the new tests, `setup` then `test`.
            let _ = std::fs::remove_dir_all(s.scratch);
            crate::git::archive_all(s.worktree, s.base_sha, s.scratch).await?;
            let files = crate::git::ls_tree(s.worktree, "HEAD", &s.cfg.namespace).await?;
            crate::git::archive_into(s.worktree, "HEAD", &files, s.scratch).await?;
            let timeout = Duration::from_secs(s.cfg.check_timeout_secs);
            let mut setup_ok = true;
            if let Some(argv) = s.cfg.checks.get("setup") {
                let r = run_one("L1", "setup", argv, s.scratch, s.sandbox, timeout, &[]).await;
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
                setup_ok = r.ok;
                v.checks.push(r);
            }
            if setup_ok {
                let argv = s.cfg.checks.get("test").cloned().unwrap_or_default();
                let mut r = run_one(
                    Rule::RedOnBase.level(),
                    Rule::RedOnBase.name(),
                    &argv,
                    s.scratch,
                    s.sandbox,
                    timeout,
                    &[],
                )
                .await;
                // The row passes when the tests FAIL on base.
                let failed_on_base = !r.ok && !r.timed_out;
                r.ok = failed_on_base;
                if !failed_on_base {
                    r.tail = format!(
                        "the new tests {} on the base commit, so they do not specify the task\n{}",
                        if r.timed_out { "timed out" } else { "pass" },
                        r.tail
                    );
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
            let _ = std::fs::remove_dir_all(s.scratch);
        }
        v.envelope = env;
    }
    let (state, reason) = decide(
        agent_reason.as_deref(),
        question.as_ref().map(|(k, q)| (*k, q.as_str())),
        &v.checks,
    );
    v.state = state;
    v.reason = reason;
    Ok(v)
}

pub struct ReviewSubject<'a> {
    pub cfg: &'a Config,
    pub task_id: i64,
    pub worktree: &'a Path,
    pub base_sha: &'a str,
    pub start_sha: &'a str,
    pub report: &'a Reporter,
}

/// The review contract's verdict. The reviewer may not change the branch
/// (no writes since it started, clean tree). It may demote the task to
/// human review only with something it executed: a demotion from a
/// session that ran no tool at all is recorded as a note and does not
/// stand.
pub async fn verify_review(s: ReviewSubject<'_>, agent: &Outcome) -> Result<Verdict> {
    let agent_reason = agent_failure(agent);
    let mut v = Verdict {
        commits: 0,
        files_changed: 0,
        dirty: false,
        envelope: None,
        checks: Vec::new(),
        state: AttemptState::Running,
        reason: String::new(),
    };
    let mut question: Option<(Kind, String)> = None;
    let (rows, env, q, commits, changed, _changed_now, dirty) = common_l0(
        s.worktree,
        s.base_sha,
        s.start_sha,
        None,
        s.cfg,
        agent,
        s.report,
        s.task_id,
    )
    .await?;
    v.commits = commits;
    v.files_changed = changed.len() as i64;
    v.dirty = !dirty.is_empty();
    if agent_reason.is_none() {
        v.checks = rows
            .into_iter()
            .filter(|r| {
                !matches!(
                    Rule::parse(&r.name),
                    Some(Rule::HasCommits | Rule::ChangesMatchGit)
                )
            })
            .collect();
        let added = crate::git::changed_paths(s.worktree, s.start_sha).await?;
        v.checks.push(l0(
            Rule::NoWrites,
            added.is_empty() && dirty.is_empty(),
            format!(
                "the reviewer changed the branch: {}",
                added
                    .iter()
                    .chain(dirty.iter())
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
        match q {
            Some((Kind::Review, text)) => {
                if agent.tool_calls > 0 {
                    question = Some((Kind::Review, text));
                } else {
                    s.report.emit(
                        s.task_id,
                        Event::Note {
                            text: &format!(
                                "review   demotion ignored (no executed evidence): {text}"
                            ),
                        },
                    );
                }
            }
            other => question = other,
        }
        v.envelope = env;
    }
    let (mut state, mut reason) = decide(
        agent_reason.as_deref(),
        question.as_ref().map(|(k, q)| (*k, q.as_str())),
        &v.checks,
    );
    // A review runs no checks of its own: the branch was verified before it
    // started. Nothing to object to means the review passed.
    if state == AttemptState::Unverified {
        state = AttemptState::Succeeded;
        reason = String::new();
    }
    v.state = state;
    v.reason = reason;
    Ok(v)
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

/// The plan contract's verdict. The investigator may not change the
/// branch and must return either a substantive plan naming only paths
/// that exist, or a question. No L1: nothing was built.
pub async fn verify_plan(s: ReviewSubject<'_>, agent: &Outcome) -> Result<Verdict> {
    let agent_reason = agent_failure(agent);
    let mut v = Verdict {
        commits: 0,
        files_changed: 0,
        dirty: false,
        envelope: None,
        checks: Vec::new(),
        state: AttemptState::Running,
        reason: String::new(),
    };
    let (rows, env, question, commits, changed, _changed_now, dirty) = common_l0(
        s.worktree,
        s.base_sha,
        s.start_sha,
        None,
        s.cfg,
        agent,
        s.report,
        s.task_id,
    )
    .await?;
    v.commits = commits;
    v.files_changed = changed.len() as i64;
    v.dirty = !dirty.is_empty();
    if agent_reason.is_none() {
        v.checks = rows
            .into_iter()
            .filter(|r| {
                matches!(
                    Rule::parse(&r.name),
                    Some(Rule::ResultStructured | Rule::CleanTree)
                )
            })
            .collect();
        let added = crate::git::changed_paths(s.worktree, s.start_sha).await?;
        v.checks.push(l0(
            Rule::Untouched,
            added.is_empty() && dirty.is_empty(),
            format!(
                "the investigator changed the branch: {}",
                added
                    .iter()
                    .chain(dirty.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
        if question.is_none() {
            let plan = env
                .as_ref()
                .map(|e| e.summary.trim().to_string())
                .unwrap_or_default();
            v.checks.push(l0(
                Rule::PlanSubstantive,
                plan.chars().count() >= 120,
                format!("a plan of {} characters is not a plan; name the files, the changes, and the test", plan.chars().count()),
            ));
            // A path the plan names must exist, or be a new file in a
            // directory that does: plans create files, they do not
            // invent directories.
            let missing: Vec<String> = plan_paths(&plan, &|d| s.worktree.join(d).is_dir())
                .into_iter()
                .filter(|p| {
                    let path = s.worktree.join(p);
                    !path.exists() && !path.parent().is_some_and(|d| d.is_dir())
                })
                .collect();
            v.checks.push(l0(
                Rule::PlanNamesRealPaths,
                missing.is_empty(),
                format!(
                    "the plan names paths that do not exist in the tree, in directories that do not exist either: {}",
                    missing.join(", ")
                ),
            ));
        }
        emit_rows(s.report, s.task_id, &v.checks);
        v.envelope = env;
    }
    let (mut state, mut reason) = decide(
        agent_reason.as_deref(),
        question.as_ref().map(|(k, q)| (*k, q.as_str())),
        &v.checks,
    );
    // A plan runs no checks of its own: nothing was built. Its L0 rows are
    // the whole verdict.
    if state == AttemptState::Unverified {
        state = AttemptState::Succeeded;
        reason = String::new();
    }
    v.state = state;
    v.reason = reason;
    Ok(v)
}

/// Why the agent run itself counts as failed, if it does.
pub fn agent_failure(a: &Outcome) -> Option<String> {
    if a.rate_limited {
        Some("rate limited by the provider".into())
    } else if let Some(why) = &a.ended_early {
        Some(format!("stopped early: {why}"))
    } else if a.timed_out {
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
/// `question` is (kind, text).
pub fn decide(
    agent_failure: Option<&str>,
    question: Option<(Kind, &str)>,
    checks: &[CheckResult],
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
    fn namespace_membership() {
        let ns = vec!["tests/acceptance/".to_string()];
        assert!(in_namespace(&ns, "tests/acceptance/a.sh"));
        assert!(!in_namespace(&ns, "tests/acceptance.sh"));
        assert!(!in_namespace(&ns, "src/a.ts"));
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
