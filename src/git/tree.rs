//! Metadata restoration and pure path/URL helpers.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Strips whatever a sandboxed launch or check may have written into
/// `<dir>/.git` before the first host git command touches that directory
/// unsandboxed: `config` back to a fresh, trusted one (never a patch to
/// what a check left), `hooks/` and `info/` emptied, and `commondir` and
/// `gitdir` removed outright rather than inspected, since either would
/// make git honor an object store, refs and config the sandbox planted
/// instead of this directory's own. `dir/.git` must already be a plain
/// directory; a symlink or gitfile in its place is a deeper compromise
/// this refuses to follow.
pub fn restore_metadata(dir: &Path) -> Result<()> {
    let git_dir = dir.join(".git");
    let meta = std::fs::symlink_metadata(&git_dir)
        .with_context(|| format!("no .git in {}", dir.display()))?;
    if !meta.is_dir() {
        bail!("{} is not a plain directory", git_dir.display());
    }
    for redirect in ["commondir", "gitdir"] {
        let _ = std::fs::remove_file(git_dir.join(redirect));
    }
    std::fs::write(
        git_dir.join("config"),
        super::KERNEL_CONFIG.replace("bare = true", "bare = false"),
    )?;
    for sub in ["hooks", "info"] {
        let d = git_dir.join(sub);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d)?;
    }
    Ok(())
}

/// Make `clone` the kernel-made `tree`: the agent's directory is emptied and
/// the tree's contents (metadata included) moved in, by plain file moves.
/// No git command runs in either.
pub fn adopt_tree(clone: &Path, tree: &Path) -> Result<()> {
    for entry in std::fs::read_dir(clone)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    for entry in std::fs::read_dir(tree)? {
        let entry = entry?;
        std::fs::rename(entry.path(), clone.join(entry.file_name()))?;
    }
    std::fs::remove_dir_all(tree)?;
    Ok(())
}

/// Resolve local remote paths relative to the repository that owns the remote,
/// never the process cwd or the integrator's separate Git directory.
pub fn local_remote_path(repo: &Path, url: &str) -> Option<PathBuf> {
    let path = if let Some(path) = url.strip_prefix("file://") {
        // Git's file transport ignores the authority (including localhost)
        // and percent-decodes the path. It does not contact that host.
        use std::os::unix::ffi::OsStringExt;
        let path = &path[path.find('/')?..];
        PathBuf::from(std::ffi::OsString::from_vec(
            percent_encoding::percent_decode_str(path).collect(),
        ))
    } else {
        // A colon before any slash denotes a URL scheme or scp-style remote.
        if url.split('/').next()?.contains(':') || url.is_empty() {
            return None;
        }
        repo.join(url)
    };
    path.canonicalize().ok()
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
