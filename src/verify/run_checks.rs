//! Running the repository's own checks: L1 then L2 with the verification
//! namespace overlaid, the deterministic known-fix repair, and red-on-base
//! for the tests contract.

use super::overlay::{overlay, overlay_label, remove_overlay};
use super::*;
use crate::checks::run_one;
use anyhow::Result;
use std::time::Duration;

/// How much of a failed L1/L2 check's combined stdout and stderr stays on
/// the row the attempt keeps forever (`verdict_json`); the rest is only on
/// disk, at `CheckResult::log_path`.
const RECORD_TAIL_BYTES: usize = 4 * 1024;

/// As `checks::run_one`, but bounding what stays on the row to
/// `RECORD_TAIL_BYTES` and writing the check's whole output to
/// `s.logs_dir`, so a failed check's record is never missing the test
/// that failed for want of room, whatever the check printed.
async fn run_one_recorded(
    s: &Subject<'_>,
    level: &str,
    name: &str,
    argv: &[String],
    cwd: &Path,
    timeout: Duration,
    env: &[(String, String)],
) -> CheckResult {
    crate::checks::run_one_capped(crate::checks::RunOneCapped {
        level,
        name,
        argv,
        cwd,
        sandbox: s.sandbox,
        timeout,
        env,
        cap_bytes: RECORD_TAIL_BYTES,
        full_log_dir: Some(s.logs_dir),
        egress: None,
    })
    .await
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
    // The checks just ran sandboxed in `wt` and could have written
    // anything into `.git`; strip it before the host git calls below.
    crate::git::restore_metadata(wt)?;
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
pub(super) async fn l1_l2(
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
        let r = run_one_recorded(s, "L1", name, argv, s.worktree, timeout, &facts).await;
        // Checks can replace Git metadata even when they fail.
        crate::git::restore_metadata(s.worktree)?;
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
            let mut r = run_one_recorded(s, "L2", &name, &argv, s.worktree, timeout, &facts).await;
            crate::git::restore_metadata(s.worktree)?;
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
    crate::git::clear_namespace(s.worktree, &s.cfg.namespace).await?;
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
pub(super) async fn try_known_fix(
    s: &Subject<'_>,
    checks: &[CheckResult],
) -> Result<Option<KnownFix>> {
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
        // A fix command is untrusted just like a check. In particular,
        // commit_all below must never read a config redirected by commondir.
        crate::git::restore_metadata(s.worktree)?;
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

/// Red on base: the base tree plus the new tests, `setup` then `test`,
/// in the scratch directory. The row passes when the tests FAIL on base.
pub(super) async fn red_on_base(s: &Subject<'_>, checks: &mut Vec<CheckResult>) -> Result<()> {
    let scratch = s
        .scratch
        .ok_or_else(|| anyhow::anyhow!("the tests contract needs a scratch directory"))?;
    let _ = std::fs::remove_dir_all(scratch);
    crate::sandbox::discard_provider_state(scratch);
    crate::git::archive_all(s.worktree, s.base_sha, scratch).await?;
    let files = crate::git::ls_tree(s.worktree, "HEAD", &s.cfg.namespace).await?;
    crate::git::archive_into(s.worktree, "HEAD", &files, scratch).await?;
    let timeout = Duration::from_secs(s.cfg.check_timeout_secs);
    let facts = s.facts();
    let mut setup_ok = true;
    if let Some(argv) = s.cfg.checks.get("setup") {
        let r = run_one_recorded(s, "L1", "setup", argv, scratch, timeout, &facts).await;
        emit_check(s.report, s.task_id, &r);
        setup_ok = r.ok;
        checks.push(r);
    }
    if setup_ok {
        let argv = s.cfg.checks.get("test").cloned().unwrap_or_default();
        let mut r = run_one_recorded(
            s,
            Rule::RedOnBase.level(),
            Rule::RedOnBase.name(),
            &argv,
            scratch,
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
    crate::sandbox::discard_provider_state(scratch);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

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
            logs_dir: dir.path(),
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
            logs_dir: dir.path(),
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
            logs_dir: dir.path(),
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
            logs_dir: dir.path(),
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
            logs_dir: dir.path(),
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

    /// docs/REVIEW-4.md #1.23: `red_on_base` must discard the scratch
    /// directory's own `-provider` sibling along with the scratch itself,
    /// not just the scratch, or a copy of the login leaks per task.
    #[tokio::test]
    async fn red_on_base_leaves_neither_the_scratch_nor_its_provider_directory() {
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        let (dir, base) = commit_fixture().await;
        let mut cfg = test_cfg();
        let report = Reporter::new(false, None);
        let scratch = dir.path().join("wt-red");
        let build_env = BTreeMap::from([("CARGO_BUILD_JOBS".into(), "2".into())]);
        crate::agent::build_env::configure_env(dir.path(), &scratch, &build_env, &BTreeMap::new());
        // Another attempt is configured before red-on-base creates its tree.
        crate::agent::build_env::configure_env(
            dir.path(),
            dir.path(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        cfg.checks.insert(
            "setup".into(),
            vec![
                "sh".into(),
                "-c".into(),
                "test \"$CARGO_BUILD_JOBS\" = 2".into(),
            ],
        );
        cfg.checks.insert(
            "test".into(),
            vec![
                "sh".into(),
                "-c".into(),
                "echo CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS; exit 1".into(),
            ],
        );
        let provider = PathBuf::from(format!("{}-provider", scratch.display()));
        std::fs::create_dir_all(provider.join("claude")).unwrap();
        std::fs::write(provider.join("claude").join("credentials.json"), "{}").unwrap();
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
            scratch: Some(&scratch),
            plan_rows: true,
        };
        let mut checks = Vec::new();
        red_on_base(&s, &mut checks).await.unwrap();
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|check| check.ok), "{checks:?}");
        assert!(checks[1].tail.contains("CARGO_BUILD_JOBS=2"));
        assert!(!scratch.exists(), "the scratch directory was left behind");
        assert!(
            !provider.exists(),
            "the scratch's provider directory was left behind"
        );
    }
}
