//! Git as mechanism. Every task works in its own single-branch clone of the
//! base branch: the registered checkout is only ever read, the clone's
//! object store never holds the verification refs, and the sandbox never
//! sees the repository's own .git.

use anyhow::{Context, Result, bail};
use std::path::Path;
use tokio::process::Command;

async fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub async fn current_branch(repo: &Path) -> Result<String> {
    git(repo, &["symbolic-ref", "--short", "HEAD"])
        .await
        .context("repo is on a detached HEAD; set defaults.base_branch in forge.toml")
}

pub async fn ref_exists(repo: &Path, full_ref: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", full_ref])
        .await
        .is_ok()
}

/// A single-branch clone of `base` from the registered checkout, on a new
/// task branch, with no remote at all: the agent inside cannot fetch
/// anything, in particular not the verification refs. Forge pushes by URL.
/// Returns the base commit.
pub async fn clone_task(repo: &Path, base: &str, dir: &Path, branch: &str) -> Result<String> {
    let dir_s = dir.to_str().context("clone path is not UTF-8")?;
    let repo_s = repo.to_str().context("repo path is not UTF-8")?;
    let out = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--single-branch",
            "--no-tags",
            "--branch",
            base,
            repo_s,
            dir_s,
        ])
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "git clone of {} at {base} failed: {}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let base_sha = git(dir, &["rev-parse", "HEAD"]).await?;
    git(dir, &["checkout", "--quiet", "-b", branch]).await?;
    git(dir, &["remote", "remove", "origin"]).await?;
    Ok(base_sha)
}

/// The content of `path` at `rev`, or `None` if it does not exist there.
pub async fn show_file(dir: &Path, rev: &str, path: &str) -> Result<Option<String>> {
    let spec = format!("{rev}:{path}");
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["show", &spec])
        .output()
        .await?;
    if out.status.success() {
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    } else {
        Ok(None)
    }
}

pub async fn count_commits(wt: &Path, base_sha: &str) -> Result<i64> {
    let range = format!("{base_sha}..HEAD");
    Ok(git(wt, &["rev-list", "--count", &range])
        .await?
        .parse()
        .unwrap_or(0))
}

/// Paths changed between base and HEAD, committed only.
pub async fn changed_paths(wt: &Path, base_sha: &str) -> Result<Vec<String>> {
    let out = git(wt, &["diff", "--name-only", base_sha, "HEAD"]).await?;
    Ok(out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Porcelain status entries: anything uncommitted, untracked included.
pub async fn dirty_paths(wt: &Path) -> Result<Vec<String>> {
    let out = git(wt, &["status", "--porcelain"]).await?;
    Ok(out
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| l[3..].to_string())
        .collect())
}

pub async fn remote_url(repo: &Path, remote: &str) -> Option<String> {
    git(repo, &["remote", "get-url", remote]).await.ok()
}

/// Whether the clone's HEAD is exactly what the remote holds for `branch`,
/// i.e. every commit it added has been published.
pub async fn published(wt: &Path, url: &str, branch: &str) -> Result<bool> {
    let head = git(wt, &["rev-parse", "HEAD"]).await?;
    let full = format!("refs/heads/{branch}");
    let out = Command::new("git")
        .args(["ls-remote", url, &full])
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "git ls-remote {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        == Some(head.as_str()))
}

/// Push exactly one branch to one remote URL by explicit refspec. Never
/// forced, never the base branch, never a deletion. Runs on the host with
/// the operator's credentials, never inside the sandbox.
pub async fn push(wt: &Path, url: &str, branch: &str) -> Result<()> {
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    git(wt, &["push", "--quiet", url, &refspec]).await?;
    Ok(())
}

/// Push a branch from a clone into the registered checkout's refs (never
/// its working tree): how the tests step publishes `verify/<id>` for the
/// kernel to overlay from. The refspec is explicit and unforced.
pub async fn push_to_repo(wt: &Path, repo: &Path, branch: &str) -> Result<()> {
    let repo_s = repo.to_str().context("repo path is not UTF-8")?;
    let refspec = format!("HEAD:refs/heads/{branch}");
    git(wt, &["push", "--quiet", repo_s, &refspec]).await?;
    Ok(())
}

/// Files under `paths` present in `rev`, for an overlay.
pub async fn ls_tree(repo: &Path, rev: &str, paths: &[String]) -> Result<Vec<String>> {
    let mut args = vec!["ls-tree", "-r", "--name-only", rev, "--"];
    let owned: Vec<&str> = paths.iter().map(String::as_str).collect();
    args.extend(owned);
    let out = git(repo, &args).await?;
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
    let mut args: Vec<String> = vec![
        "-C".into(),
        repo.display().to_string(),
        "archive".into(),
        "--format=tar".into(),
        rev.into(),
        "--".into(),
    ];
    args.extend(files.iter().cloned());
    let tar = Command::new("git").args(&args).output().await?;
    if !tar.status.success() {
        bail!(
            "git archive {rev} failed: {}",
            String::from_utf8_lossy(&tar.stderr).trim()
        );
    }
    let mut untar = Command::new("tar")
        .args(["-x", "-C"])
        .arg(dest)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = untar.stdin.take().context("tar stdin")?;
        stdin.write_all(&tar.stdout).await?;
        stdin.shutdown().await?;
    }
    let status = untar.wait().await?;
    if !status.success() {
        bail!("tar extraction into {} failed", dest.display());
    }
    Ok(())
}

/// The committer identity the clone would use, for passing into the sandbox
/// where ~/.gitconfig is invisible.
pub async fn identity(git_dir: &Path) -> Vec<(String, String)> {
    let get = |key: &'static str| async move {
        Command::new("git")
            .arg("--git-dir")
            .arg(git_dir)
            .args(["config", "--get", key])
            .output()
            .await
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let name = get("user.name")
        .await
        .unwrap_or_else(|| "Forge".to_string());
    let email = get("user.email")
        .await
        .unwrap_or_else(|| "forge@localhost".to_string());
    vec![
        ("GIT_CONFIG_COUNT".into(), "2".into()),
        ("GIT_CONFIG_KEY_0".into(), "user.name".into()),
        ("GIT_CONFIG_VALUE_0".into(), name),
        ("GIT_CONFIG_KEY_1".into(), "user.email".into()),
        ("GIT_CONFIG_VALUE_1".into(), email),
    ]
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
    Command::new("git")
        .args(["ls-remote", "--heads", url, &full])
        .output()
        .await
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// The whole tree at `rev` extracted into `dest`, with no git state.
pub async fn archive_all(repo: &Path, rev: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let tar = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["archive", "--format=tar", rev])
        .output()
        .await?;
    if !tar.status.success() {
        bail!(
            "git archive {rev} failed: {}",
            String::from_utf8_lossy(&tar.stderr).trim()
        );
    }
    let mut untar = Command::new("tar")
        .args(["-x", "-C"])
        .arg(dest)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = untar.stdin.take().context("tar stdin")?;
        stdin.write_all(&tar.stdout).await?;
        stdin.shutdown().await?;
    }
    if !untar.wait().await?.success() {
        bail!("tar extraction into {} failed", dest.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::compare_url;

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
}
