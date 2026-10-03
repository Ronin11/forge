//! The verdict entry points: an operation, an integration landing, and an
//! agent's directive, each assembling the shared L0 rows, the contract's
//! own rows, and L1/L2, then deciding.

use super::l0_checks::{common_l0, no_stray_files, scope_rows};
use super::overlay::{in_namespace, overlay_note};
use super::run_checks::{l1_l2, red_on_base, try_known_fix};
use super::*;
use crate::envelope;
use crate::workflows::Contract;
use anyhow::Result;

async fn remote_verdict(s: &Subject<'_>) -> Result<Option<Verdict>> {
    if !s
        .sandbox
        .is_some_and(|e| e.backend(s.worktree) == crate::executor::Backend::Ssh)
    {
        return Ok(None);
    }
    let facts = GitFacts {
        commits: crate::git::count_commits(s.worktree, s.base_sha).await?,
        changed: crate::git::changed_paths(s.worktree, s.base_sha).await?,
        changed_now: crate::git::changed_paths(s.worktree, s.start_sha).await?,
        dirty: crate::git::dirty_paths(s.worktree).await?,
    };
    let mut verdict = Verdict::open(&facts);
    verdict.state = AttemptState::Unverified;
    verdict.reason = "remote executor: the kernel could not run the checks itself".into();
    Ok(Some(verdict))
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
        format!(
            "left uncommitted by the operation: {}{}",
            dirty.join(", "),
            overlay_note(dirty, &s.cfg.namespace)
        ),
    ));
    v.checks
        .push(no_stray_files(s.worktree, s.start_sha).await?);
    v.checks
        .extend(scope_rows(&s, changed, changed, dirty).await?);
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
    if let Some(verdict) = remote_verdict(s).await? {
        return Ok(verdict);
    }
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
        format!(
            "uncommitted after the merge: {}{}",
            dirty.join(", "),
            overlay_note(dirty, &s.cfg.namespace)
        ),
    ));
    let touched = !s.allow_protected && changed.iter().any(|p| p == s.cfg.config_path.as_str());
    v.checks.push(l0(
        Rule::ConfigUntouched,
        !touched,
        format!("the merged tree modifies {}", s.cfg.config_path),
    ));
    v.checks
        .extend(scope_rows(s, changed, changed, dirty).await?);
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
/// - review: no writes, and a demotion stands only with something run
///   and a reproduction a fresh clone can run (`review::rows`).
/// - plan: untouched, and a plan that is substantive and names real paths.
pub async fn verify_directive(
    contract: Contract,
    s: &Subject<'_>,
    agent: &Outcome,
) -> Result<Verdict> {
    if let Some(mut verdict) = remote_verdict(s).await? {
        verdict.envelope = envelope::parse(agent.structured.as_deref(), &agent.result_text)
            .ok()
            .flatten();
        return Ok(verdict);
    }
    let notes = match contract {
        Contract::Review => review::capture_notes(s).await?,
        _ => Vec::new(),
    };
    let mut common = common_l0(s, agent).await?;
    let (agent_reason, recovered) = recovery::resolve(s, agent, &mut common).await?;
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
                v.checks
                    .extend(scope_rows(s, &facts.changed, &facts.changed_now, &facts.dirty).await?);
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
                let env = common.envelope.as_ref();
                review::rows(s, agent, facts, env, &notes, &mut question, &mut v.checks).await?;
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
        // The notes travel with the demotion they reproduce.
        if let (Some(e), Some((Kind::Review, _))) = (&mut v.envelope, &question) {
            e.review_notes = notes;
        }
    }
    v.settle(
        agent_reason.as_deref(),
        question.as_ref().map(|(k, q)| (*k, q.as_str())),
        contract.verifies_work(),
    );
    if recovered {
        if v.state == AttemptState::Succeeded {
            v.envelope.as_mut().unwrap().summary = "envelope missing; verified by checks".into();
        } else {
            v.envelope = None;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

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
            logs_dir: dir.path(),
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

    mod allow_protected;
    mod namespace;

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
}
