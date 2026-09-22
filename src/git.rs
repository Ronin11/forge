//! Git as mechanism. Every task works in its own single-branch clone of the
//! base branch: the registered checkout is only ever read, the clone's
//! object store never holds the verification refs, and the sandbox never
//! sees the repository's own .git.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// The committer identity Forge commits as, wherever no better identity is
/// configured.
const IDENTITY: (&str, &str) = ("Forge", "forge@localhost");

/// A git invocation bound to one directory, with an optional committer
/// identity (`-c user.name=... -c user.email=...`) and extra environment
/// variables. Every git spawn in this module goes through it.
struct Git {
    dir: PathBuf,
    identity: bool,
    env: Vec<(String, String)>,
    env_remove: Vec<String>,
}

impl Git {
    fn new(dir: impl Into<PathBuf>) -> Self {
        Git {
            dir: dir.into(),
            identity: false,
            env: Vec::new(),
            env_remove: Vec::new(),
        }
    }

    fn with_identity(mut self) -> Self {
        self.identity = true;
        self
    }

    fn with_env(mut self, env: impl IntoIterator<Item = (String, String)>) -> Self {
        self.env.extend(env);
        self
    }

    /// Hides `keys` from this spawn alone, via `Command::env_remove` rather
    /// than a process-wide `std::env::remove_var`: a test isolating itself
    /// from ambient `GIT_CONFIG_*` must not blind every other git spawn
    /// racing it in the same test binary in the meantime.
    #[cfg(test)]
    fn without_env(mut self, keys: impl IntoIterator<Item = &'static str>) -> Self {
        self.env_remove.extend(keys.into_iter().map(String::from));
        self
    }

    /// The raw `Output`, for callers that need the exit status or stdout
    /// bytes directly rather than a `Result<String>`.
    async fn output(&self, args: &[&str]) -> Result<std::process::Output> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.dir);
        if self.identity {
            cmd.args(["-c", &format!("user.name={}", IDENTITY.0)]);
            cmd.args(["-c", &format!("user.email={}", IDENTITY.1)]);
        }
        cmd.args(args).envs(self.env.iter().map(|(k, v)| (k, v)));
        for k in &self.env_remove {
            cmd.env_remove(k);
        }
        cmd.kill_on_drop(true);
        cmd.output()
            .await
            .with_context(|| format!("running git {}", args.join(" ")))
    }

    /// Untrimmed stdout on success, for output whose leading whitespace
    /// carries meaning (porcelain status).
    async fn raw(&self, args: &[&str]) -> Result<String> {
        let out = self.output(args).await?;
        if !out.status.success() {
            bail!(
                "git {} failed in {}: {}",
                args.join(" "),
                self.dir.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Trimmed stdout on success.
    async fn line(&self, args: &[&str]) -> Result<String> {
        Ok(self.raw(args).await?.trim().to_string())
    }
}

/// The paths in `git status --porcelain` output, one parser for every
/// caller: the two status columns and a space, then the path; a first
/// line whose leading space was trimmed still yields its path.
pub fn porcelain_paths(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim_end)
        .filter(|l| l.len() > 2)
        .map(|l| {
            let b = l.as_bytes();
            // Untrimmed: two status columns and a space. Trimmed: the
            // first column was a space and is gone, so the path follows
            // the first space.
            if l.starts_with("?? ") || b[0] == b' ' || (l.len() > 3 && b[2] == b' ' && b[1] != b' ')
            {
                l[3..].to_string()
            } else {
                l.split_once(' ').map(|(_, p)| p).unwrap_or(l).to_string()
            }
        })
        .collect()
}

pub async fn current_branch(repo: &Path) -> Result<String> {
    Git::new(repo)
        .line(&["symbolic-ref", "--short", "HEAD"])
        .await
        .context("repo is on a detached HEAD; set defaults.base_branch in forge.toml")
}

pub async fn ref_exists(repo: &Path, full_ref: &str) -> bool {
    Git::new(repo)
        .line(&["rev-parse", "--verify", "--quiet", full_ref])
        .await
        .is_ok()
}

/// A single-branch clone of `base` from the registered checkout, on a new
/// task branch, with no remote at all: the agent inside cannot fetch
/// anything, in particular not the verification refs. Forge pushes by URL.
/// Returns the base commit.
pub async fn clone_task(
    repo: &Path,
    base: &str,
    dir: &Path,
    branch: &str,
    at_ref: Option<&str>,
    at_sha: Option<&str>,
) -> Result<String> {
    let dir_s = dir.to_str().context("clone path is not UTF-8")?;
    let repo_s = repo.to_str().context("repo path is not UTF-8")?;
    // The clone target does not exist yet, so run from the source repo,
    // which does.
    Git::new(repo)
        .raw(&[
            "clone",
            "--quiet",
            "--single-branch",
            "--no-tags",
            "--branch",
            base,
            repo_s,
            dir_s,
        ])
        .await?;
    let dir_git = Git::new(dir);
    // The base as the remote has it, not as the registered checkout has it;
    // a recorded sha wins over the ref, so every clone of one task agrees.
    match (at_ref, at_sha) {
        (Some(r), sha) => {
            let fetched = dir_git.line(&["fetch", "--quiet", repo_s, r]).await.is_ok();
            match (fetched, sha) {
                (true, Some(sha)) | (false, Some(sha)) => {
                    dir_git.line(&["reset", "--hard", "--quiet", sha]).await?;
                }
                (true, None) => {
                    dir_git
                        .line(&["reset", "--hard", "--quiet", "FETCH_HEAD"])
                        .await?;
                }
                (false, None) => bail!("fetch of {r} from {} failed", repo.display()),
            }
        }
        (None, Some(sha)) => {
            dir_git.line(&["reset", "--hard", "--quiet", sha]).await?;
        }
        (None, None) => {}
    }
    let base_sha = dir_git.line(&["rev-parse", "HEAD"]).await?;
    dir_git.line(&["checkout", "--quiet", "-b", branch]).await?;
    dir_git.line(&["remote", "remove", "origin"]).await?;
    Ok(base_sha)
}

/// Fetch one branch from any source (a path or a URL) into `dir` as FETCH_HEAD.
pub async fn fetch_ref(dir: &Path, src: &str, branch: &str) -> Result<()> {
    Git::new(dir)
        .line(&["fetch", "--quiet", src, &format!("refs/heads/{branch}")])
        .await?;
    Ok(())
}

/// Bring one branch of a remote up to date in the registered checkout's
/// remote-tracking refs, without touching any local branch. The sha.
pub async fn fetch_branch(repo: &Path, remote: &str, branch: &str) -> Result<String> {
    let g = Git::new(repo);
    g.line(&["fetch", "--quiet", remote, branch]).await?;
    g.line(&["rev-parse", &format!("refs/remotes/{remote}/{branch}")])
        .await
}

pub async fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    Git::new(repo)
        .line(&["rev-parse", "--verify", "--quiet", rev])
        .await
}

/// Put a commit of `repo` into the task's clone as a local branch, so an
/// agent with no remote can merge it. Forced: the branch is the kernel's.
pub async fn place_branch(repo: &Path, dir: &Path, sha: &str, branch: &str) -> Result<()> {
    let dir_s = dir.to_str().context("clone path is not UTF-8")?;
    let refspec = format!("+{sha}:refs/heads/{branch}");
    Git::new(repo)
        .line(&["push", "--quiet", dir_s, &refspec])
        .await?;
    Ok(())
}

pub enum Merge {
    /// Already contained the commit; nothing to do.
    UpToDate,
    /// Merged cleanly; the new HEAD.
    Merged(String),
    /// Conflicts, aborted; the tree is as it was.
    Conflict(Vec<String>),
}

/// Merge `rev` into the current branch as Forge. A conflict is aborted and
/// reported, never left in the tree.
pub async fn merge(dir: &Path, rev: &str, message: &str) -> Result<Merge> {
    let g = Git::new(dir);
    if g.line(&["merge-base", "--is-ancestor", rev, "HEAD"])
        .await
        .is_ok()
    {
        return Ok(Merge::UpToDate);
    }
    let out = Git::new(dir)
        .with_identity()
        .output(&["merge", "--quiet", "--no-edit", "-m", message, rev])
        .await?;
    if out.status.success() {
        return Ok(Merge::Merged(g.line(&["rev-parse", "HEAD"]).await?));
    }
    let conflicted: Vec<String> = g
        .line(&["diff", "--name-only", "--diff-filter=U"])
        .await
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let _ = g.line(&["merge", "--abort"]).await;
    if conflicted.is_empty() {
        bail!(
            "git merge of {rev} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(Merge::Conflict(conflicted))
}

pub async fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> bool {
    Git::new(dir)
        .line(&["merge-base", "--is-ancestor", ancestor, descendant])
        .await
        .is_ok()
}

/// Fast-forward `branch` on the remote to the clone's HEAD. Never forced:
/// a branch that moved underneath rejects the push and the caller retries.
pub async fn push_head_to(wt: &Path, url: &str, branch: &str) -> Result<()> {
    let refspec = format!("HEAD:refs/heads/{branch}");
    Git::new(wt)
        .line(&["push", "--quiet", url, &refspec])
        .await?;
    Ok(())
}

/// Files whose net change on the branch differs between two bases: what a
/// merge or a conflict resolution actually did, as opposed to what it
/// brought in from the other side. A file's net change is its diff from
/// the base to the tip; the same diff against the new base means the
/// merge only carried the file through.
pub async fn net_changes(
    wt: &Path,
    old_base: &str,
    old_tip: &str,
    new_base: &str,
    new_tip: &str,
) -> Result<Vec<String>> {
    let before = changed_paths_between(wt, old_base, old_tip).await?;
    let after = changed_paths_between(wt, new_base, new_tip).await?;
    let mut files: Vec<String> = before.iter().chain(after.iter()).cloned().collect();
    files.sort();
    files.dedup();
    let mut out = Vec::new();
    for f in files {
        let a = file_patch(wt, old_base, old_tip, &f).await?;
        let b = file_patch(wt, new_base, new_tip, &f).await?;
        if a != b {
            out.push(f);
        }
    }
    Ok(out)
}

async fn changed_paths_between(wt: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    let out = Git::new(wt)
        .line(&["diff", "--name-only", from, to])
        .await?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// One file's patch between two commits, without the volatile index line.
async fn file_patch(wt: &Path, from: &str, to: &str, path: &str) -> Result<String> {
    let out = Git::new(wt).line(&["diff", from, to, "--", path]).await?;
    Ok(out
        .lines()
        .filter(|l| !l.starts_with("index "))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Copy `files` as they are at `from_ref` onto the tip of `branch` as one
/// commit, creating the branch from an empty tree when it does not exist.
/// Plumbing only: no checkout, no working tree. The new commit, or `None`
/// when the branch already had exactly those contents.
pub async fn graft(
    repo: &Path,
    from_ref: &str,
    files: &[String],
    branch: &str,
    message: &str,
) -> Result<Option<String>> {
    let g = Git::new(repo);
    let full = format!("refs/heads/{branch}");
    let parent = g
        .line(&["rev-parse", "--verify", "--quiet", &full])
        .await
        .ok();
    let index = repo
        .join(".git")
        .join(format!("forge-graft-{}.index", std::process::id()));
    let index_s = index
        .to_str()
        .context("index path is not UTF-8")?
        .to_string();
    let _ = std::fs::remove_file(&index);
    let indexed = Git::new(repo)
        .with_identity()
        .with_env([("GIT_INDEX_FILE".to_string(), index_s)]);
    let run = |args: Vec<String>| {
        let indexed = &indexed;
        async move {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            indexed.line(&refs).await
        }
    };
    let result: Result<Option<String>> = async {
        match &parent {
            Some(p) => {
                run(vec!["read-tree".into(), p.clone()]).await?;
            }
            None => {
                run(vec!["read-tree".into(), "--empty".into()]).await?;
            }
        }
        for f in files {
            let entry = g.line(&["ls-tree", from_ref, "--", f]).await?;
            // "<mode> blob <sha>\t<path>"
            let Some((meta, _)) = entry.split_once('\t') else {
                continue;
            };
            let parts: Vec<&str> = meta.split_whitespace().collect();
            if parts.len() != 3 {
                continue;
            }
            run(vec![
                "update-index".into(),
                "--add".into(),
                "--cacheinfo".into(),
                format!("{},{},{}", parts[0], parts[2], f),
            ])
            .await?;
        }
        let tree = run(vec!["write-tree".into()]).await?;
        if let Some(p) = &parent
            && g.line(&["rev-parse", &format!("{p}^{{tree}}")]).await? == tree
        {
            return Ok(None);
        }
        let mut args = vec!["commit-tree".into(), tree, "-m".into(), message.to_string()];
        if let Some(p) = &parent {
            args.push("-p".into());
            args.push(p.clone());
        }
        let commit = run(args).await?;
        g.line(&["update-ref", &full, &commit]).await?;
        Ok(Some(commit))
    }
    .await;
    let _ = std::fs::remove_file(&index);
    result
}

/// Stage everything and commit as Forge, for what an operation changed.
/// Returns the new commit, or `None` when there was nothing to commit.
/// Ignored files stay ignored, as they do for the agent.
pub async fn commit_all(dir: &Path, message: &str) -> Result<Option<String>> {
    let g = Git::new(dir);
    g.line(&["add", "-A"]).await?;
    let staged = g.output(&["diff", "--cached", "--quiet"]).await?;
    if staged.status.success() {
        return Ok(None);
    }
    Git::new(dir)
        .with_identity()
        .line(&["commit", "--quiet", "--no-verify", "-m", message])
        .await?;
    Ok(Some(g.line(&["rev-parse", "HEAD"]).await?))
}

/// Stage one path, relative to `dir`, and commit as Forge — unlike
/// `commit_all`, nothing else already dirty in `dir` rides along, which
/// matters for a directory a person edits by hand alongside Forge (the
/// operator's workflow catalog: `forge workflows put` writing one file
/// must never sweep up some other in-progress, unvetted edit sitting
/// next to it). Returns the new commit, or `None` when `path` was
/// already exactly this content at `HEAD`.
pub async fn commit_path(dir: &Path, path: &str, message: &str) -> Result<Option<String>> {
    let g = Git::new(dir);
    g.line(&["add", "--", path]).await?;
    let staged = g
        .output(&["diff", "--cached", "--quiet", "--", path])
        .await?;
    if staged.status.success() {
        return Ok(None);
    }
    Git::new(dir)
        .with_identity()
        .line(&[
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            message,
            "--",
            path,
        ])
        .await?;
    Ok(Some(g.line(&["rev-parse", "HEAD"]).await?))
}

/// Make `dir` a fresh repository holding everything already in it as one
/// commit, and return that commit: how `forge job test` gives its scratch
/// copy of a working tree the revision the job executor archives from.
pub async fn init_commit_all(dir: &Path, message: &str) -> Result<String> {
    Git::new(dir).line(&["init", "--quiet"]).await?;
    commit_all(dir, message)
        .await?
        .with_context(|| format!("nothing to commit in {}", dir.display()))
}

/// Discard every commit and change on the branch back to `sha`.
pub async fn reset_hard(dir: &Path, sha: &str) -> Result<()> {
    let g = Git::new(dir);
    g.line(&["reset", "--hard", "--quiet", sha]).await?;
    g.line(&["clean", "-fdq"]).await?;
    Ok(())
}

pub async fn head(dir: &Path) -> Result<String> {
    Git::new(dir).line(&["rev-parse", "HEAD"]).await
}

/// The content of `path` at `rev`, or `None` if it does not exist there.
pub async fn show_file(dir: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    let spec = format!("{rev}:{path}");
    let out = Git::new(dir).output(&["show", &spec]).await?;
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

pub async fn count_commits(wt: &Path, base_sha: &str) -> Result<i64> {
    let range = format!("{base_sha}..HEAD");
    Ok(Git::new(wt)
        .line(&["rev-list", "--count", &range])
        .await?
        .parse()
        .unwrap_or(0))
}

/// Paths changed between base and HEAD, committed only.
pub async fn changed_paths(wt: &Path, base_sha: &str) -> Result<Vec<String>> {
    let out = Git::new(wt)
        .line(&["diff", "--name-only", base_sha, "HEAD"])
        .await?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Added and removed content lines between two commits, as `(path,
/// content)` pairs with the `+`/`-` marker stripped: what a landing's
/// diff actually put in the tree and took out of it, for the delayed-cost
/// churn measurement (docs/LATER.md). A zero-context diff (`--unified=0`)
/// so a line is only "added" or "removed" here when its exact content
/// changed, not because it happened to sit near a change. Duplicates are
/// kept (an identical line added twice counts twice), and a binary file's
/// change contributes nothing to either side.
pub async fn diff_lines(
    repo: &Path,
    from: &str,
    to: &str,
) -> Result<(Vec<(String, String)>, Vec<(String, String)>)> {
    let raw = Git::new(repo)
        .raw(&["diff", "--unified=0", "--no-color", from, to])
        .await?;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut old_path = String::new();
    let mut new_path = String::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            old_path = rest.strip_prefix("a/").unwrap_or(rest).to_string();
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            new_path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
        } else if let Some(content) = line.strip_prefix('-') {
            removed.push((old_path.clone(), content.to_string()));
        } else if let Some(content) = line.strip_prefix('+') {
            added.push((new_path.clone(), content.to_string()));
        }
    }
    Ok((added, removed))
}

/// One line of `git diff --name-status -M`: a plain add, modify or
/// delete, or a rename with both endpoints (git detected the same
/// content moving, at or above the default similarity threshold).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitChange {
    Added(String),
    Modified(String),
    Deleted(String),
    Renamed { from: String, to: String },
}

fn parse_status_line(line: &str) -> Option<GitChange> {
    let mut parts = line.split('\t');
    let code = parts.next()?;
    match code.as_bytes().first()? {
        b'A' => Some(GitChange::Added(parts.next()?.to_string())),
        b'M' => Some(GitChange::Modified(parts.next()?.to_string())),
        b'D' => Some(GitChange::Deleted(parts.next()?.to_string())),
        b'R' => Some(GitChange::Renamed {
            from: parts.next()?.to_string(),
            to: parts.next()?.to_string(),
        }),
        // A copy reads as a new file at its destination.
        b'C' => Some(GitChange::Added(parts.nth(1)?.to_string())),
        _ => None,
    }
}

/// Paths changed between `from` and `to`, with rename detection forced
/// (`-M`) so a move reads as one `Renamed` entry rather than a delete and
/// an add that happen to land in the same diff.
pub async fn changed_with_status(wt: &Path, from: &str, to: &str) -> Result<Vec<GitChange>> {
    let out = Git::new(wt)
        .line(&[
            "-c",
            "core.quotepath=off",
            "diff",
            "--name-status",
            "-M",
            from,
            to,
        ])
        .await?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(parse_status_line)
        .collect())
}

/// The full unified diff from `from` to `to`, as a human or an agent
/// reviewer would read it (unlike `diff_lines`, which strips markers for
/// the churn measurement).
pub async fn diff_text(dir: &Path, from: &str, to: &str) -> Result<String> {
    Git::new(dir).raw(&["diff", from, to]).await
}

/// `git diff --shortstat`'s one line ("2 files changed, 3 insertions(+), 1
/// deletion(-)"), for the diff stat a known fix's operation row carries.
pub async fn diff_shortstat(dir: &Path, from: &str, to: &str) -> Result<String> {
    Ok(Git::new(dir)
        .line(&["diff", "--shortstat", from, to])
        .await?
        .trim()
        .to_string())
}

/// Porcelain status entries: anything uncommitted, untracked included.
pub async fn dirty_paths(wt: &Path) -> Result<Vec<String>> {
    Ok(porcelain_paths(
        &Git::new(wt).raw(&["status", "--porcelain"]).await?,
    ))
}

pub async fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    Git::new(repo)
        .line(&["remote", "get-url", remote])
        .await
        .ok()
}

/// Whether the clone's HEAD is exactly what the remote holds for `branch`,
/// i.e. every commit it added has been published.
pub async fn published(wt: &Path, url: &str, branch: &str) -> Result<bool> {
    let g = Git::new(wt);
    let head = g.line(&["rev-parse", "HEAD"]).await?;
    let full = format!("refs/heads/{branch}");
    let out = g.raw(&["ls-remote", url, &full]).await?;
    Ok(out.split_whitespace().next() == Some(head.as_str()))
}

/// Push exactly one branch to one remote URL by explicit refspec. Never
/// forced, never the base branch, never a deletion. Runs on the host with
/// the operator's credentials, never inside the sandbox.
pub async fn push(wt: &Path, url: &str, branch: &str) -> Result<()> {
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    Git::new(wt)
        .line(&["push", "--quiet", url, &refspec])
        .await?;
    Ok(())
}

/// Push a branch from a clone into the registered checkout's refs (never
/// its working tree): how the tests step publishes `verify/<id>` for the
/// kernel to overlay from. The refspec is explicit and unforced.
pub async fn push_to_repo(wt: &Path, repo: &Path, branch: &str) -> Result<()> {
    let repo_s = repo.to_str().context("repo path is not UTF-8")?;
    let refspec = format!("HEAD:refs/heads/{branch}");
    Git::new(wt)
        .line(&["push", "--quiet", repo_s, &refspec])
        .await?;
    Ok(())
}

/// Files under `paths` present in `rev`, for an overlay.
pub async fn ls_tree(repo: &Path, rev: &str, paths: &[String]) -> Result<Vec<String>> {
    let mut args = vec!["ls-tree", "-r", "--name-only", rev, "--"];
    let owned: Vec<&str> = paths.iter().map(String::as_str).collect();
    args.extend(owned);
    let out = Git::new(repo).line(&args).await?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Extract `files` at `rev` from `repo` into `dest`, without touching
/// `dest`'s git state: the files simply appear on disk.
pub async fn archive_into(repo: &Path, rev: &str, files: &[String], dest: &Path) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut args: Vec<&str> = vec!["archive", "--format=tar", rev, "--"];
    let owned: Vec<&str> = files.iter().map(String::as_str).collect();
    args.extend(owned);
    let g = Git::new(repo);
    let tar = g.output(&args).await?;
    if !tar.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&tar.stderr).trim()
        );
    }
    untar(&tar.stdout, dest).await
}

/// The whole tree at `rev` extracted into `dest`, with no git state.
/// A fresh scratch checkout of `rev`: `dest` removed if it exists, then
/// the whole tree archived into it. A deploy, a job and a verifying
/// operation each kept their own copy of these two lines
/// (docs/REVIEW-2.md, theme 2.1).
pub async fn fresh_archive(repo: &Path, rev: &str, dest: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(dest);
    archive_all(repo, rev, dest).await
}

pub async fn archive_all(repo: &Path, rev: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let args = ["archive", "--format=tar", rev];
    let g = Git::new(repo);
    let tar = g.output(&args).await?;
    if !tar.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&tar.stderr).trim()
        );
    }
    untar(&tar.stdout, dest).await
}

/// Pipe a tar stream into `tar -x`, extracting it under `dest`.
async fn untar(tar: &[u8], dest: &Path) -> Result<()> {
    let mut child = Command::new("tar")
        .args(["-x", "-C"])
        .arg(dest)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().context("tar stdin")?;
        stdin.write_all(tar).await?;
        stdin.shutdown().await?;
    }
    if !child.wait().await?.success() {
        bail!("tar extraction into {} failed", dest.display());
    }
    Ok(())
}

/// One git config key, falling back to `default` when the key is unset or
/// empty.
async fn config_or(g: &Git, key: &str, default: &str) -> String {
    g.line(&["config", "--get", key])
        .await
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// The committer identity the clone would use, for passing into the sandbox
/// where ~/.gitconfig is invisible.
pub async fn identity(git_dir: &Path) -> Vec<(String, String)> {
    let g = Git::new(git_dir);
    let name = config_or(&g, "user.name", IDENTITY.0).await;
    let email = config_or(&g, "user.email", IDENTITY.1).await;
    vec![
        ("GIT_CONFIG_COUNT".into(), "2".into()),
        ("GIT_CONFIG_KEY_0".into(), "user.name".into()),
        ("GIT_CONFIG_VALUE_0".into(), name),
        ("GIT_CONFIG_KEY_1".into(), "user.email".into()),
        ("GIT_CONFIG_VALUE_1".into(), email),
    ]
}

/// Commits in `(from, to]` whose author is not the identity Forge would
/// commit as in `repo` (the repository's own `user.name`/`user.email`
/// config, falling back to `IDENTITY`, the same resolution `identity`
/// uses): hand commits that reached the base branch outside any task Forge
/// ran, for the human-attention measurement (docs/LATER.md, "Two metrics
/// the record can compute and does not").
pub async fn hand_commit_count(repo: &Path, from: &str, to: &str) -> Result<i64> {
    let g = Git::new(repo);
    let name = config_or(&g, "user.name", IDENTITY.0).await;
    let email = config_or(&g, "user.email", IDENTITY.1).await;
    let range = format!("{from}..{to}");
    let raw = g.raw(&["log", "--format=%an%x1f%ae", &range]).await?;
    Ok(raw
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| {
            let mut parts = l.splitn(2, '\u{1f}');
            let author_name = parts.next().unwrap_or("");
            let author_email = parts.next().unwrap_or("");
            author_name != name || author_email != email
        })
        .count() as i64)
}

/// A compare URL for GitHub-shaped remotes; `None` for anything else.
pub fn compare_url(remote_url: &str, base: &str, branch: &str) -> Option<String> {
    let rest = remote_url
        .strip_prefix("git@github.com:")
        .or_else(|| remote_url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| remote_url.strip_prefix("https://github.com/"))?;
    let path = rest.trim_end_matches('/').trim_end_matches(".git");
    Some(format!(
        "https://github.com/{path}/compare/{base}...{branch}?expand=1"
    ))
}

/// Whether `branch` already exists on the remote at `url`.
pub async fn remote_branch_exists(url: &str, branch: &str) -> bool {
    let full = format!("refs/heads/{branch}");
    Git::new(".")
        .output(&["ls-remote", "--heads", url, &full])
        .await
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_remotes_get_compare_urls() {
        let want = "https://github.com/nate/repo/compare/main...forge/7-x?expand=1";
        for url in [
            "git@github.com:nate/repo.git",
            "https://github.com/nate/repo",
            "ssh://git@github.com/nate/repo.git",
        ] {
            assert_eq!(
                compare_url(url, "main", "forge/7-x").as_deref(),
                Some(want),
                "{url}"
            );
        }
    }

    #[test]
    fn other_remotes_get_none() {
        assert_eq!(compare_url("/srv/git/repo.git", "main", "b"), None);
        assert_eq!(
            compare_url("git@gitlab.com:nate/repo.git", "main", "b"),
            None
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .status()
            .unwrap();
        dir
    }

    #[tokio::test]
    async fn a_failing_git_call_reports_the_args_and_dir() {
        let dir = init_repo();
        let err = Git::new(dir.path())
            .line(&["rev-parse", "--verify", "does-not-exist"])
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.starts_with("git rev-parse --verify does-not-exist failed in "),
            "{msg}"
        );
        assert!(msg.contains(&dir.path().display().to_string()), "{msg}");
    }

    #[tokio::test]
    async fn diff_lines_reports_content_added_and_removed_by_path() {
        let dir = init_repo();
        let wt = dir.path();
        std::fs::write(wt.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let base = commit_all(wt, "base").await.unwrap().unwrap();
        std::fs::write(wt.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        let tip = commit_all(wt, "edit").await.unwrap().unwrap();

        let (added, removed) = diff_lines(wt, &base, &tip).await.unwrap();
        assert_eq!(
            added,
            vec![
                ("a.txt".to_string(), "TWO".to_string()),
                ("a.txt".to_string(), "four".to_string()),
            ]
        );
        assert_eq!(removed, vec![("a.txt".to_string(), "two".to_string())]);
    }

    #[tokio::test]
    async fn commit_path_leaves_other_dirty_files_uncommitted() {
        let dir = init_repo();
        let wt = dir.path();
        std::fs::write(wt.join("a.toml"), "a\n").unwrap();
        std::fs::write(wt.join("b.toml"), "b\n").unwrap();
        let sha = commit_path(wt, "a.toml", "add a").await.unwrap().unwrap();
        let status = Git::new(wt).line(&["status", "--porcelain"]).await.unwrap();
        assert_eq!(status, "?? b.toml");
        let log = Git::new(wt)
            .line(&["log", "--format=%s", &sha])
            .await
            .unwrap();
        assert_eq!(log, "add a");
        let files = Git::new(wt)
            .line(&["show", "--name-only", "--format=", &sha])
            .await
            .unwrap();
        assert_eq!(files, "a.toml");
    }

    #[tokio::test]
    async fn commit_path_is_none_when_that_path_is_unchanged() {
        let dir = init_repo();
        let wt = dir.path();
        std::fs::write(wt.join("a.toml"), "a\n").unwrap();
        commit_path(wt, "a.toml", "add a").await.unwrap().unwrap();
        let again = commit_path(wt, "a.toml", "add a again").await.unwrap();
        assert_eq!(again, None);
    }

    #[tokio::test]
    async fn identity_falls_back_to_the_constant_when_unset() {
        let dir = init_repo();
        let git_dir = dir.path().join(".git");
        // Isolated per spawn, via `Command::env_remove`/`.env()`, not a
        // process-wide `std::env::set_var`: this is the same test binary
        // every other test in it runs in, and a global mutation here would
        // blind or redirect any git spawn racing it — `agent::run_codex`'s
        // own dirty/head checks among them, which read the ambient
        // environment because they carry no identity of their own.
        let g = Git::new(&git_dir)
            .without_env([
                "GIT_CONFIG_COUNT",
                "GIT_CONFIG_KEY_0",
                "GIT_CONFIG_VALUE_0",
                "GIT_CONFIG_KEY_1",
                "GIT_CONFIG_VALUE_1",
            ])
            .with_env([
                ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
                ("GIT_CONFIG_SYSTEM".to_string(), "/dev/null".to_string()),
            ]);
        assert_eq!(config_or(&g, "user.name", IDENTITY.0).await, IDENTITY.0);
        assert_eq!(config_or(&g, "user.email", IDENTITY.1).await, IDENTITY.1);
    }
}
