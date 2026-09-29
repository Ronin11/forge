//! L0: what every step shares before any check runs — the agent's result
//! parsed against git, and where a change is allowed to land.

use super::overlay::{in_namespace, overlay_note};
use super::*;
use crate::config::is_protected;
use crate::envelope::{self, Change};
use anyhow::Result;

/// The report's `changes[]`, filled from git instead of the model: every
/// path this attempt added, modified, deleted or renamed since it started,
/// read the same way `changes_match_git` reconciles a report against git.
/// A rename is split into its two endpoints, so it reads like any other
/// add and delete a model would have written by hand.
pub(super) async fn derive_changes(wt: &Path, start_sha: &str) -> Result<Vec<Change>> {
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
pub(super) async fn no_stray_files(wt: &Path, start_sha: &str) -> Result<CheckResult> {
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
pub(super) async fn common_l0(s: &Subject<'_>, agent: &Outcome) -> Result<Common> {
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
        format!(
            "uncommitted: {}{}",
            dirty.join(", "),
            overlay_note(&dirty, &cfg.namespace)
        ),
    ));
    rows.push(no_stray_files(worktree, start_sha).await?);
    let touched = !s.allow_protected
        && changed
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

/// The L0 rows about where a change landed: protected paths, the
/// directive's write scope, the verification namespace.
/// `changed` is the branch's whole change, for the rules that guard the
/// product; `changed_now` is this step's, for the directive's own write
/// scope: a scoped step after an unscoped one is judged on what it did.
pub(super) async fn scope_rows(
    s: &Subject<'_>,
    changed: &[String],
    changed_now: &[String],
    dirty: &[String],
) -> Result<Vec<CheckResult>> {
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
        let touched = crate::git::committed_paths(s.worktree, s.base_sha).await?;
        let hit: Vec<&str> = touched
            .iter()
            .chain(dirty.iter())
            .map(String::as_str)
            .filter(|p| in_namespace(&s.cfg.namespace, p))
            .collect();
        rows.push(l0(
            Rule::NamespaceUntouched,
            hit.is_empty(),
            format!("commits or working tree touch the verification namespace: {}. That namespace is reserved for the tests that judge this work.", hit.join(", ")),
        ));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

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
            logs_dir: dir.path(),
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
}
