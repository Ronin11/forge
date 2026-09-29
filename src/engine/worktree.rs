//! The task's worktree: cloning it fresh, or resuming a verified branch
//! from an earlier attempt at the same task, including unverified work.

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
/// task with unlanded commits or a verified branch, that branch with the current base
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
        if let Some(old) = branch_parent(f, t)? {
            merged_base_retry = reuse_branch(f, t, old, &dir).await?;
        }
    }
    Ok(merged_base_retry)
}

/// Refiles keep a fresh accounting lineage but inherit their source checkout.
fn branch_parent(f: &Forge, t: &Task) -> Result<Option<i64>, Fault> {
    match t.retry_of {
        Some(id) => Ok(Some(id)),
        None => f.store.refile_source(t.id).env(),
    }
}

fn reusable_branch(verified: bool, has_unlanded_commits: bool) -> bool {
    verified || has_unlanded_commits
}

async fn reuse_branch(f: &Forge, t: &mut Task, mut old: i64, dir: &Path) -> Result<bool, Fault> {
    let mut seen = HashSet::new();
    while seen.insert(old) {
        let Some(parent) = f.store.task(old).env()? else {
            break;
        };
        // A task can only inherit work from the same registered repository.
        if parent.repo != t.repo {
            break;
        }
        let attempts = f.store.attempts(old).env()?;
        let verified = attempts
            .iter()
            .rev()
            .find(|a| a.is_agent())
            .is_some_and(|last| {
                let passed = last.state == AttemptState::Succeeded
                    || (last.state == AttemptState::NeedsInput
                        && (last.reason.starts_with("review demoted")
                            || verify::l1_all_passed(
                                &serde_json::from_str::<Vec<CheckResult>>(&last.verdict_json)
                                    .unwrap_or_default(),
                            )));
                passed && attempts.iter().any(|a| a.commits > 0)
            });
        if let Some(source) = branch_source(&parent).await {
            match git::fetch_ref(dir, &source, &parent.branch).await {
                Ok(()) => {
                    let tip = git::rev_parse(dir, "FETCH_HEAD").await.task()?;
                    let unlanded = !git::is_ancestor(dir, &tip, &t.base_sha).await;
                    if reusable_branch(verified, unlanded) {
                        return start_branch(f, t, &parent, &tip, verified, dir).await;
                    }
                }
                Err(e) => f.report.emit(t.id, Event::Note { text: &format!(
                    "start    task {old}'s branch {} could not be fetched ({e:#}); trying its predecessor", parent.branch) }),
            }
        }
        let Some(next) = branch_parent(f, &parent)? else {
            break;
        };
        old = next;
    }
    Ok(false)
}

async fn start_branch(
    f: &Forge,
    t: &mut Task,
    parent: &Task,
    tip: &str,
    verified: bool,
    dir: &Path,
) -> Result<bool, Fault> {
    git::reset_hard(dir, tip).await.task()?;
    let fork = git::merge_base(dir, &t.base_sha, tip)
        .await
        .unwrap_or(t.base_sha.clone());
    let files = git::changed_paths(dir, &fork).await.task()?;
    let subjects = git::log_oneline(dir, &t.base_sha).await.task()?;
    let summary = format!(
        "Predecessor task {} branch {} @ {}\nFiles: {}\nCommit subjects:\n{}",
        parent.id,
        parent.branch,
        tip,
        files.join(", "),
        subjects
    );
    let label = if verified {
        "verified branch"
    } else {
        "unlanded branch"
    };
    let mut note = format!(
        "start    from task {}'s {label} {} @ {}",
        parent.id,
        parent.branch,
        &tip[..tip.len().min(8)]
    );
    let mut merged = false;
    if !git::is_ancestor(dir, &t.base_sha, "HEAD").await {
        let msg = format!("Merge the current base into {}", parent.branch);
        match git::merge(dir, &t.base_sha, &msg).await {
            Ok(git::Merge::Merged(_)) | Ok(git::Merge::UpToDate) => {
                merged = true;
                note.push_str(", with the current base merged in");
            }
            result => {
                let conflict = match result {
                    Ok(git::Merge::Conflict(files)) => format!("conflicts in {}", files.join(", ")),
                    Err(e) => format!("merge failed: {e:#}"),
                    _ => unreachable!(),
                };
                git::reset_hard(dir, &t.base_sha).await.task()?;
                note = format!(
                    "start    task {}'s branch {} {conflict}; starting from the base. Cherry-pick the needed commits from {}",
                    parent.id, parent.branch, parent.branch
                );
            }
        }
    }
    f.report.emit(t.id, Event::Note { text: &note });
    t.task.push_str(&format!("\n\n{summary}\n{note}"));
    f.store.update_task(t).env()?;
    Ok(merged)
}

async fn branch_source(parent: &Task) -> Option<String> {
    if parent.branch.is_empty() {
        return None;
    }
    // Prefer the published copy: an operator may have fixed it by hand.
    if parent.pushed {
        let repo = Path::new(&parent.repo);
        if let Ok(cfg) = config::load_working(repo).await
            && let Some(remote) = cfg.push_remote
            && let Some(url) = git::remote_url(repo, &remote).await
        {
            return Some(url);
        }
    }
    if Path::new(&parent.worktree).join(".git").exists() {
        return Some(parent.worktree.clone());
    }
    None
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
