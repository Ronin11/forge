//! The task's worktree: cloning it fresh, or resuming a verified branch
//! from an earlier attempt at the same task.

use super::*;

/// A branch-safe slug from the first few words of the task text.
pub fn slug(task: &str) -> String {
    let mut out = String::new();
    for word in task.split_whitespace().take(5) {
        let w: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if w.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&w);
    }
    out.chars()
        .take(32)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// The task's worktree: a fresh clone of the base (fetched from the remote
/// when there is one) on a branch named for the task, or, for a retry of a
/// task whose branch passed the checks, that branch with the current base
/// merged in. Returns whether such a merge happened: a textually clean
/// merge that does not build surfaces at the setup step, and that failure
/// belongs to the coder, not to the task, since a fresh clone would never
/// see it. Does nothing for a resumed task that already has a worktree.
pub(super) async fn prepare_worktree(
    f: &Forge,
    t: &mut Task,
    repo: &Path,
    base_cfg: &config::Config,
    remote_url: &Option<String>,
) -> Result<bool, Fault> {
    let id = t.id;
    let seq: i64 = 0;
    // A verified-branch retry merged with the current base may fail setup;
    // that failure belongs to the coder, since a fresh clone would build.
    let mut merged_base_retry = false;
    crate::git::clear_recorded_overlay(&t.worktree);
    if t.worktree.is_empty() {
        let base_name = format!("forge/{}-{}", t.id, slug(&t.task));
        t.branch = base_name.clone();
        if let Some(url) = &remote_url {
            for k in 2.. {
                if !git::remote_branch_exists(url, &t.branch).await.env()? {
                    break;
                }
                t.branch = format!("{base_name}-{k}");
            }
        }
        let dir = f.paths.worktrees.join(t.id.to_string());
        // An unrecorded clone can only be debris from an interrupted run.
        if dir.exists() {
            std::fs::remove_dir_all(&dir).env()?;
        }
        let timer = Timer::now();
        // The base is the remote's, so a task started after a landing sees it.
        let base_ref = match (&base_cfg.push_remote, &remote_url) {
            (Some(name), Some(url))
                if git::remote_branch_exists(url, &t.base_branch).await.env()? =>
            {
                match git::fetch_branch(repo, name, &t.base_branch).await {
                    Ok(_) => Some(format!("refs/remotes/{name}/{}", t.base_branch)),
                    Err(e) => {
                        f.report.emit(
                            id,
                            Event::Note {
                                text: &format!(
                                    "fetch    {name}/{} failed ({e:#}); using the local base",
                                    t.base_branch
                                ),
                            },
                        );
                        None
                    }
                }
            }
            _ => None,
        };
        let r = git::clone_task(
            repo,
            &t.base_branch,
            &dir,
            &t.branch,
            base_ref.as_deref(),
            None,
        )
        .await;
        op(
            f,
            id,
            &timer,
            OpRow {
                seq,
                name: "clone",
                kernel: true,
                ok: r.is_ok(),
                exit: None,
                detail: &r
                    .as_ref()
                    .map(|s| s[..8].to_string())
                    .unwrap_or_else(|e| format!("{e:#}")),
                attempt_id: None,
                output: "",
            },
        )?;
        t.base_sha = r.env()?;
        // The standing hidden suite as it matches this base; a suite that
        // grows while the task runs is for the landing, not for the coder.
        t.verify_base = git::rev_parse(repo, "refs/heads/forge-verify")
            .await
            .unwrap_or_default();
        t.worktree = dir.display().to_string();
        f.store.update_task(t).env()?;
        // Reuse a verified branch: only the review finding or operator's
        // answer remains to address, avoiding repeated rebuilds from scratch.
        if let Some(old) = t.retry_of
            && let Some(from) = verified_branch_of(f, old).await
        {
            match git::fetch_ref(&dir, &from.source, &from.branch).await {
                Ok(()) => {
                    let tip = git::rev_parse(&dir, "FETCH_HEAD").await.unwrap_or_default();
                    let short = &tip[..tip.len().min(8)];
                    git::reset_hard(&dir, "FETCH_HEAD").await.task()?;
                    if git::is_ancestor(&dir, &t.base_sha, "HEAD").await {
                        f.report.emit(id, Event::Note { text: &format!("start    from task {old}'s verified branch {} @ {short}", from.branch) });
                    } else {
                        // Main moved: merge the current base as at landing,
                        // preserving verified work unless the merge fails.
                        let msg = format!("Merge the current base into {}", from.branch);
                        match git::merge(&dir, &t.base_sha, &msg).await {
                            Ok(git::Merge::Merged(_)) | Ok(git::Merge::UpToDate) => {
                                merged_base_retry = true;
                                f.report.emit(id, Event::Note { text: &format!("start    from task {old}'s verified branch {} @ {short}, with the current base merged in", from.branch) });
                            }
                            Ok(git::Merge::Conflict(files)) => {
                                git::reset_hard(&dir, &t.base_sha).await.task()?;
                                f.report.emit(id, Event::Note { text: &format!("start    task {old}'s branch conflicts with the current base in {}; starting fresh", files.join(", ")) });
                            }
                            Err(e) => {
                                git::reset_hard(&dir, &t.base_sha).await.task()?;
                                f.report.emit(id, Event::Note { text: &format!("start    could not merge the current base into task {old}'s branch ({e:#}); starting fresh") });
                            }
                        }
                    }
                }
                Err(e) => f.report.emit(
                    id,
                    Event::Note {
                        text: &format!("start    task {old}'s branch could not be fetched ({e:#}); starting fresh"),
                    },
                ),
            }
        }
    }
    Ok(merged_base_retry)
}

/// Where a retry may start from: the parent's branch, when the parent's
/// last real attempt passed the checks (verified, or verified and then
/// demoted by a reviewer) and its clone or its pushed branch still exists.
struct VerifiedBranch {
    source: String,
    branch: String,
}

async fn verified_branch_of(f: &Forge, old: i64) -> Option<VerifiedBranch> {
    // Walk up the retry chain: a retry that itself failed (a rebuild
    // that capped, an integrate the coder could not settle) still has a
    // verified ancestor whose branch is the right place to start.
    let mut id = old;
    let parent = loop {
        let parent = f.store.task(id).ok().flatten()?;
        let attempts = f.store.attempts(id).ok()?;
        let verified = !parent.branch.is_empty()
            && attempts
                .iter()
                .rev()
                .find(|a| a.is_agent())
                .is_some_and(|last| {
                    let ok = match last.state {
                        AttemptState::Succeeded => true,
                        // A demotion the operator or the supervisor set
                        // aside, or a question the repository's checks
                        // already ran and passed on: neither settled the
                        // attempt, but both leave a branch worth resuming
                        // from rather than rebuilding.
                        AttemptState::NeedsInput => {
                            last.reason.starts_with("review demoted")
                                || verify::l1_all_passed(
                                    &serde_json::from_str::<Vec<CheckResult>>(&last.verdict_json)
                                        .unwrap_or_default(),
                                )
                        }
                        _ => false,
                    };
                    ok && (last.commits > 0 || attempts.iter().any(|a| a.commits > 0))
                });
        if verified {
            break parent;
        }
        id = parent.retry_of?;
    };
    // The pushed branch first: it is the copy a person can fix by hand,
    // and the local worktree goes stale the moment someone does (task
    // 269 fetched a worktree that predated the fix on the remote).
    if parent.pushed {
        let repo = Path::new(&parent.repo);
        if let Ok(cfg) = config::load_working(repo).await
            && let Some(remote) = cfg.push_remote
            && let Some(url) = git::remote_url(repo, &remote).await
        {
            return Some(VerifiedBranch {
                source: url,
                branch: parent.branch.clone(),
            });
        }
    }
    if Path::new(&parent.worktree).join(".git").exists() {
        return Some(VerifiedBranch {
            source: parent.worktree.clone(),
            branch: parent.branch.clone(),
        });
    }
    None
}
