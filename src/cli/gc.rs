use super::*;

/// Nothing with unpublished work is deleted, except when `older_than` says
/// otherwise: a retained worktree whose task finished at least that many
/// days ago is pushed (to the remote when the repo has one, else into the
/// registered repository, `publish`'s own choice in `engine.rs`) and then
/// removed even unpublished, so nothing sits on disk forever just because
/// it was never merged. A worktree also goes when it is clean and every
/// commit it added is already reachable from a remote ref (or it added
/// none). Everything else is kept with the reason and the command a human
/// would run. Branches are never deleted, and a pushed branch lands without
/// its worktree (`forge land` recreates it), so removing one loses nothing.
pub(super) async fn gc(dry_run: bool, older_than: Option<i64>) -> Result<()> {
    let f = Forge::open(false, false)?;
    let (mut removed, mut kept) = (0, 0);
    let now = crate::unix_now();
    for t in f.store.tasks_with_worktrees()? {
        let wt = Path::new(&t.worktree);
        let verdict: Result<Result<(), String>> = async {
            if !wt.exists() {
                return Ok(Ok(()));
            }
            if t.state == TaskState::Running {
                return Ok(Err("still running".into()));
            }
            if !git::dirty_paths(wt).await?.is_empty() {
                return Ok(Err("uncommitted changes".into()));
            }
            let commits = git::count_commits(wt, &t.base_sha).await?;
            // Unpublished commits are kept, unless a later try of the same
            // piece of work succeeded: then they are superseded, not lost.
            let superseded = f
                .store
                .lineage(t.id)?
                .iter()
                .any(|l| l.id > t.id && l.state == "succeeded");
            if commits > 0
                && !superseded
                && let Err(reason) =
                    gc_publish(&f.paths.home, &t, wt, commits, older_than, dry_run, now).await?
            {
                return Ok(Err(reason));
            }
            if !dry_run {
                std::fs::remove_dir_all(wt)?;
                crate::sandbox::discard_provider_state(wt);
                let tests_clone = crate::attempt::tests_clone_dir(&t.worktree);
                let _ = std::fs::remove_dir_all(&tests_clone);
                crate::sandbox::discard_provider_state(&tests_clone);
            }
            Ok(Ok(()))
        }
        .await;
        match verdict {
            Ok(Ok(())) => {
                removed += 1;
                if !dry_run {
                    f.store.mark_worktree_removed(t.id)?;
                }
                out!(
                    "task {:<4} {} {}",
                    t.id,
                    if dry_run {
                        "would remove"
                    } else {
                        "removed     "
                    },
                    t.worktree
                );
            }
            Ok(Err(reason)) => {
                kept += 1;
                out!("task {:<4} kept ({reason})", t.id);
                out!("           rm -rf {}", t.worktree);
                out!(
                    "           once its branch is pushed, forge land no longer needs this worktree"
                );
            }
            Err(e) => {
                kept += 1;
                out!("task {:<4} kept (error: {e:#})", t.id);
            }
        }
    }
    out!(
        "{} {removed}, kept {kept}",
        if dry_run { "would remove" } else { "removed" }
    );
    Ok(())
}

/// Whether a worktree with unpublished commits still blocks removal:
/// `Ok(())` once its branch is published, either already or because it is
/// old enough that `older_than` says to push it now (to the remote when
/// the repo has one, else into the registered repository — `publish`'s
/// own choice in `engine.rs`); `Err` with the reason to keep it otherwise.
async fn gc_publish(
    home: &Path,
    t: &Task,
    wt: &Path,
    commits: i64,
    older_than: Option<i64>,
    dry_run: bool,
    now: i64,
) -> Result<Result<(), String>> {
    let repo = Path::new(&t.repo);
    let url = match config::load_working(repo).await?.push_remote {
        Some(name) => git::remote_url(repo, &name).await,
        None => None,
    };
    let mut published = match &url {
        Some(u) => git::published(home, repo, wt, u, &t.branch).await?,
        None => false,
    };
    let old_enough = older_than.is_some_and(|days| {
        t.finished_at
            .is_some_and(|fin| now.saturating_sub(fin) >= days * 86_400)
    });
    if !published && old_enough && !dry_run {
        let _ = match &url {
            Some(u) => git::push(home, repo, wt, u, &t.branch).await?,
            None => git::push_to_repo(home, repo, wt, &t.branch).await?,
        };
        published = true;
    }
    if published {
        return Ok(Ok(()));
    }
    let suffix = if old_enough {
        " (would push then remove, dry run)"
    } else {
        ""
    };
    Ok(Err(format!(
        "{commits} commit(s) not on the remote{suffix}"
    )))
}
