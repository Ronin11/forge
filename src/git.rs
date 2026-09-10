//! Git as mechanism. The only operations run inside the registered checkout
//! are `worktree add/remove/prune` and read-only queries; everything else
//! runs in the attempt's own worktree.

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

pub async fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .is_ok()
}

pub async fn worktree_add(repo: &Path, wt: &Path, branch: &str, base: &str) -> Result<()> {
    let wt_s = wt.to_str().context("worktree path is not UTF-8")?;
    git(repo, &["worktree", "add", "-b", branch, wt_s, base]).await?;
    Ok(())
}

pub async fn worktree_remove(repo: &Path, wt: &Path) -> Result<()> {
    let wt_s = wt.to_str().context("worktree path is not UTF-8")?;
    git(repo, &["worktree", "remove", wt_s]).await?;
    Ok(())
}

pub async fn worktree_prune(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"]).await?;
    Ok(())
}

pub async fn rev_parse(dir: &Path, rev: &str) -> Result<String> {
    git(dir, &["rev-parse", rev]).await
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

/// Whether the worktree's HEAD is reachable from any remote-tracking ref,
/// i.e. every commit it added has been published somewhere.
pub async fn remote_contains_head(wt: &Path) -> Result<bool> {
    Ok(!git(wt, &["branch", "-r", "--contains", "HEAD"])
        .await?
        .is_empty())
}

/// Push exactly one branch to one remote by explicit refspec. Never forced,
/// never the base branch, never a deletion. Runs on the host with the
/// operator's credentials, never inside the sandbox.
pub async fn push(wt: &Path, remote: &str, branch: &str) -> Result<()> {
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    git(wt, &["push", remote, &refspec]).await?;
    Ok(())
}

/// The committer identity the repo would use, for passing into the sandbox
/// where ~/.gitconfig is invisible.
pub async fn identity(repo_git_dir: &Path) -> Vec<(String, String)> {
    let get = |key: &'static str| async move {
        Command::new("git")
            .arg("--git-dir")
            .arg(repo_git_dir)
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
