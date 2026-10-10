//! Worktree housekeeping: what the worker does once an hour and `forge gc`
//! does by hand and aloud. Landed tasks lose their trees at the terminal
//! write (`disk::task_finished`); this is the rest: a finished task's tree
//! kept `[worker] worktree_retention_days`, then its leftovers committed,
//! its branch published and the tree removed; husks and orphan siblings
//! removed; build caches swept.
use crate::ctx::Forge;
use crate::store::{Task, TaskState};
use crate::{config, disk, git};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

/// One pass. `manual` is `forge gc`: besides what retention expires, it
/// also removes a clean tree whose commits are already on the remote.
/// `older_than` (days) replaces the configured retention for this pass,
/// and `now` is the clock, so a test controls it.
pub struct Pass {
    pub dry_run: bool,
    pub manual: bool,
    pub older_than: Option<i64>,
    pub retention_days: i64,
    pub now: i64,
}

#[derive(Default)]
pub struct Report {
    pub freed: u64,
    pub removed: usize,
    pub kept: usize,
    pub lines: Vec<String>,
}

impl Report {
    /// The worker's one log line.
    pub fn summary(&self) -> String {
        format!(
            "freed {} bytes, removed {} trees, kept {}",
            self.freed, self.removed, self.kept
        )
    }
}

enum Verdict {
    Remove(&'static str),
    Keep(String),
}

pub async fn run(f: &Forge, p: &Pass) -> Result<Report> {
    let mut r = Report::default();
    for t in f.store.tasks_with_worktrees()? {
        let verdict = match judge(f, &t, p).await {
            Ok(Verdict::Remove(note)) => remove(f, &t, p, note, &mut r),
            Ok(Verdict::Keep(why)) => Verdict::Keep(why),
            Err(e) => Verdict::Keep(format!("error: {e:#}")),
        };
        if let Verdict::Keep(why) = verdict {
            r.kept += 1;
            r.lines.push(format!("task {:<4} kept ({why})", t.id));
            r.lines.push(format!("           rm -rf {}", t.worktree));
        }
    }
    orphans(f, p, &mut r)?;
    r.freed += disk::sweep(&f.store, &f.paths.worktrees, p.dry_run)?;
    Ok(r)
}

fn remove(f: &Forge, t: &Task, p: &Pass, note: &str, r: &mut Report) -> Verdict {
    let bytes = disk::task_tree_bytes(t.id, &t.worktree);
    if !p.dry_run {
        let done = disk::remove_task_trees(t.id, &t.worktree)
            .map_err(anyhow::Error::from)
            .and_then(|()| f.store.mark_worktree_removed(t.id));
        if let Err(e) = done {
            return Verdict::Keep(format!("error: {e:#}"));
        }
    }
    r.freed += bytes;
    r.removed += 1;
    let verb = if p.dry_run {
        "would remove"
    } else {
        "removed     "
    };
    let note = if note.is_empty() {
        String::new()
    } else {
        format!(" ({note})")
    };
    r.lines
        .push(format!("task {:<4} {verb} {}{note}", t.id, t.worktree));
    Verdict::Remove("")
}

async fn judge(f: &Forge, t: &Task, p: &Pass) -> Result<Verdict> {
    let wt = Path::new(&t.worktree);
    match t.state {
        TaskState::Running => return Ok(Verdict::Keep("still running".into())),
        TaskState::Queued => return Ok(Verdict::Keep("queued".into())),
        _ => {}
    }
    if !wt.exists() {
        return Ok(Verdict::Remove("tree already gone"));
    }
    if t.state == TaskState::Succeeded && !t.landed_sha.is_empty() {
        return Ok(Verdict::Remove("landed"));
    }
    if !wt.join(".git").exists() {
        return Ok(Verdict::Remove("no .git: not a worktree"));
    }
    let days = p.older_than.unwrap_or(p.retention_days);
    let finished = t.finished_at.unwrap_or(t.created_at);
    let age = p.now.saturating_sub(finished);
    if age >= days.saturating_mul(86_400) {
        return expired(f, t, p).await;
    }
    if !p.manual {
        let left = days - age / 86_400;
        return Ok(Verdict::Keep(format!("retained, {left} day(s) left")));
    }
    if !git::dirty_paths(wt).await?.is_empty() {
        return Ok(Verdict::Keep("uncommitted changes".into()));
    }
    let commits = git::count_commits(wt, &t.base_sha).await?;
    if commits > 0 && !superseded(f, t)? && !published(f, t, wt).await? {
        return Ok(Verdict::Keep(format!(
            "{commits} commit(s) not on the remote"
        )));
    }
    Ok(Verdict::Remove(""))
}

/// A later try of the same piece of work succeeded: the commits are
/// superseded, not lost.
fn superseded(f: &Forge, t: &Task) -> Result<bool> {
    Ok(f.store
        .lineage(t.id)?
        .iter()
        .any(|l| l.id > t.id && l.state == "succeeded"))
}

/// The project's push remote URL, when it has one.
async fn remote(t: &Task) -> Result<Option<String>> {
    let repo = Path::new(&t.repo);
    Ok(match config::load_working(repo).await?.push_remote {
        Some(name) => git::remote_url(repo, &name).await,
        None => None,
    })
}

async fn published(f: &Forge, t: &Task, wt: &Path) -> Result<bool> {
    match remote(t).await? {
        Some(url) => git::published(&f.paths.home, Path::new(&t.repo), wt, &url, &t.branch).await,
        None => Ok(false),
    }
}

/// Past retention: leftovers committed as a WIP commit on the task branch,
/// the branch published (to the remote when the project has one, else into
/// the registered repository), then the tree may go. A publish that fails
/// keeps the tree, so nothing with work is lost.
async fn expired(f: &Forge, t: &Task, p: &Pass) -> Result<Verdict> {
    let wt = Path::new(&t.worktree);
    if superseded(f, t)? {
        return Ok(Verdict::Remove("superseded by a later success"));
    }
    let dirty = !git::dirty_paths(wt).await?.is_empty();
    if p.dry_run {
        let commits = git::count_commits(wt, &t.base_sha).await?;
        let note = match (dirty, commits > 0) {
            (true, _) => "would commit leftovers, publish the branch",
            (false, true) => "would publish the branch",
            (false, false) => "",
        };
        return Ok(Verdict::Remove(note));
    }
    if dirty {
        let message = format!("forge: leftovers at gc, task {}", t.id);
        git::commit_all(wt, &message).await?;
    }
    let commits = git::count_commits(wt, &t.base_sha).await?;
    if commits > 0 && !published(f, t, wt).await? {
        let repo = Path::new(&t.repo);
        let pushed = match remote(t).await? {
            Some(url) => git::push(&f.paths.home, repo, wt, &url, &t.branch).await,
            None => git::push_to_repo(&f.paths.home, repo, wt, &t.branch).await,
        };
        if let Err(e) = pushed {
            return Ok(Verdict::Keep(format!(
                "publishing {} failed: {e:#}",
                t.branch
            )));
        }
    }
    Ok(Verdict::Remove(if commits > 0 {
        "branch published"
    } else {
        ""
    }))
}

/// The task a sibling-set base name (`12`, `landing-12`) belongs to.
fn base_id(base: &str) -> Option<i64> {
    base.strip_prefix("landing-").unwrap_or(base).parse().ok()
}

/// `12-overlays`, `12-tests-overlays` or `landing-12-tmp` read as their
/// base name, `12` or `landing-12`.
fn base_of(name: &str) -> Option<String> {
    let mut rest = name;
    let mut stripped = false;
    while let Some(next) = disk::SIBLINGS.iter().find_map(|s| rest.strip_suffix(s)) {
        rest = next;
        stripped = true;
    }
    (stripped && base_id(rest).is_some()).then(|| rest.to_string())
}

/// Sibling sets whose task directory is gone, and `landing-<id>` trees of
/// tasks that are not running: removed once idle for the landing grace.
fn orphans(f: &Forge, p: &Pass, r: &mut Report) -> Result<()> {
    let root = &f.paths.worktrees;
    let mut bases = BTreeSet::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(base) = base_of(&name) {
            bases.insert(base);
        } else if base_id(&name).is_some() && name.starts_with("landing-") {
            bases.insert(name);
        }
    }
    for base in bases {
        let dir = root.join(&base);
        let landing = base.starts_with("landing-");
        let id = base_id(&base).unwrap_or(0);
        if (!landing && dir.exists())
            || f.store
                .task(id)?
                .is_some_and(|t| t.state == TaskState::Running)
            || (landing && !idle(root, &base, p.now))
        {
            continue;
        }
        let bytes: u64 = std::iter::once(dir.clone())
            .chain(disk::SIBLINGS.iter().map(|s| disk::suffixed(&dir, s)))
            .chain(["-tests", "-red"].iter().flat_map(|a| {
                let clone = disk::suffixed(&dir, a);
                disk::SIBLINGS
                    .iter()
                    .map(move |s| disk::suffixed(&clone, s))
            }))
            .map(|path| disk::size(&path))
            .sum();
        if !p.dry_run {
            disk::remove_tree_set(&dir, landing)?;
        }
        r.freed += bytes;
        r.removed += 1;
        let verb = if p.dry_run {
            "would remove"
        } else {
            "removed     "
        };
        let what = if landing {
            "leftover landing tree"
        } else {
            "orphan siblings"
        };
        r.lines.push(format!("{what} {verb} {}", dir.display()));
    }
    Ok(())
}

/// Nothing under `root` named for `base` was touched within the grace.
fn idle(root: &Path, base: &str, now: i64) -> bool {
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            base_of(&e.file_name().to_string_lossy()).as_deref() == Some(base)
                || e.file_name().to_string_lossy() == base
        })
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .filter_map(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .all(|m| now - m.as_secs() as i64 >= disk::LANDING_GRACE_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_names_read_as_their_base() {
        assert_eq!(base_of("12-overlays").as_deref(), Some("12"));
        assert_eq!(base_of("12-tests-overlays").as_deref(), Some("12"));
        assert_eq!(base_of("12-review-provider").as_deref(), Some("12"));
        assert_eq!(base_of("landing-9-tmp").as_deref(), Some("landing-9"));
        assert_eq!(base_of("12"), None);
        assert_eq!(base_of("job-3-input"), None);
        assert_eq!(base_of("upgrade-tmp"), None);
    }
}
