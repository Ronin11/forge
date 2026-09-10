//! Git as mechanism. The only operations run inside the registered checkout
//! are `worktree add` and read-only queries; everything else runs in the
//! attempt's own worktree.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}:\n{}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn current_branch(repo: &Path) -> Result<String> {
    git(repo, &["symbolic-ref", "--short", "HEAD"])
        .context("repo is on a detached HEAD; set defaults.base_branch")
}

pub fn worktree_add(repo: &Path, wt: &Path, branch: &str, base: &str) -> Result<()> {
    let wt_s = wt.to_str().context("worktree path is not UTF-8")?;
    git(repo, &["worktree", "add", "-b", branch, wt_s, base])?;
    Ok(())
}

pub fn rev_parse(dir: &Path, rev: &str) -> Result<String> {
    git(dir, &["rev-parse", rev])
}

pub fn count_commits(wt: &Path, base_sha: &str) -> Result<i64> {
    let range = format!("{base_sha}..HEAD");
    Ok(git(wt, &["rev-list", "--count", &range])?
        .parse()
        .unwrap_or(0))
}

pub fn files_changed(wt: &Path, base_sha: &str) -> Result<i64> {
    let out = git(wt, &["diff", "--name-only", base_sha, "HEAD"])?;
    Ok(out.lines().filter(|l| !l.is_empty()).count() as i64)
}

pub fn is_dirty(wt: &Path) -> Result<bool> {
    Ok(!git(wt, &["status", "--porcelain"])?.is_empty())
}
