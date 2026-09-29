//! The landing guard: a pre-receive hook installed into a project's bare
//! origin (`forge init`, or `forge project guard <project>` for a project
//! registered afterward) that rejects any update to the base branch —
//! including its deletion — unless the push carries the push option
//! `forge-integrator=<token>`, a per-home secret only the integrator
//! reads. Hand work still reaches the base through `forge adopt`
//! (docs/OPS.md, "Landing hand-made work"); the hook's rejection message
//! says so. A loud emergency override, `forge-override=<reason>`, is
//! accepted in its place and recorded as a decision the hook reports to
//! `forge guard record-override` (see `deploy/pre-receive.guard`).

use crate::store::Store;
use crate::{config, git};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The reviewed hook text; installed verbatim, like `init::MIRROR_HOOK`.
pub const HOOK: &str = include_str!("../deploy/pre-receive.guard");

/// `FORGE_HOME/forge-integrator.token`.
pub fn token_path(home: &Path) -> PathBuf {
    home.join("forge-integrator.token")
}

/// The per-home integrator token: read if it already exists, else 32 bytes
/// of OS randomness written as hex, mode 0600 (the same shape as
/// `cli::web_token`). Callers on the landing path call this on every push,
/// so it never needs a separate provisioning step.
pub fn ensure_token(home: &Path) -> Result<String> {
    use std::io::{ErrorKind, Write};
    use std::os::unix::fs::PermissionsExt;

    let path = token_path(home);
    match std::fs::read_to_string(&path) {
        Ok(t) => {
            let t = t.trim().to_string();
            anyhow::ensure!(!t.is_empty(), "empty integrator token: {}", path.display());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            return Ok(t);
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .context("reading /dev/urandom")?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::create_dir_all(home)?;
    // Publish a complete, private file without replacing a token another
    // installer or integrator created while we were reading randomness.
    let mut pending = tempfile::NamedTempFile::new_in(home)?;
    pending
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    pending.write_all(t.as_bytes())?;
    match pending.persist_noclobber(&path) {
        Ok(_) => Ok(t),
        Err(e) if e.error.kind() == ErrorKind::AlreadyExists => ensure_token(home),
        Err(e) => Err(e.error).with_context(|| format!("writing {}", path.display())),
    }
}

/// Whether `bare`'s pre-receive hook is exactly the guard's reviewed text
/// (`deploy/pre-receive.guard`), the way `forge doctor` checks it: a
/// different, missing, or non-executable hook is reported as absent, even
/// if some other hook happens to sit there.
pub fn installed(bare: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    // Use Git's effective hook path, as installation does, including
    // absolute and repository-relative core.hooksPath settings.
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(bare)
        .args(["rev-parse", "--git-path", "hooks/pre-receive"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let path = String::from_utf8_lossy(&output.stdout);
    let hook = bare.join(path.trim());
    let Ok(metadata) = std::fs::metadata(&hook) else {
        return false;
    };
    metadata.is_file()
        && metadata.permissions().mode() & 0o111 != 0
        && std::fs::read_to_string(hook).ok().as_deref() == Some(HOOK)
}

/// Install `HOOK` as `bare`'s pre-receive hook (honoring its
/// `core.hooksPath`), turn on `receive.advertisePushOptions`, and record
/// what the hook cannot otherwise know: `forge.home`, `forge.repo` (named
/// in its rejection message) and `forge.base-branch`. A different hook
/// already there is kept once as `pre-receive.before-forge`, the same
/// courtesy `install_mirror_hook` extends the post-update hook. Returns
/// whether anything changed.
pub async fn install(bare: &Path, home: &Path, repo: &str, base_branch: &str) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let hooks = git::hooks_dir(bare).await?;
    std::fs::create_dir_all(&hooks)
        .with_context(|| format!("creating guard hook directory {}", hooks.display()))?;
    let hook = hooks.join("pre-receive");
    let mut changed = false;
    let old = std::fs::read_to_string(&hook).ok();
    if old.as_deref() != Some(HOOK) {
        let backup = hooks.join("pre-receive.before-forge");
        if old.is_some() && !backup.exists() {
            std::fs::rename(&hook, &backup)?;
        }
        std::fs::write(&hook, HOOK)?;
        changed = true;
    }
    let mode = std::fs::metadata(&hook)?.permissions().mode();
    if mode & 0o111 != 0o111 {
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(mode | 0o755))?;
        changed = true;
    }
    if git::config_get(bare, "receive.advertisePushOptions")
        .await
        .as_deref()
        != Some("true")
    {
        git::config_set(bare, "receive.advertisePushOptions", "true").await?;
        changed = true;
    }
    if git::config_get(bare, "forge.home").await.as_deref() != Some(&home.display().to_string()) {
        git::config_set(bare, "forge.home", &home.display().to_string()).await?;
        changed = true;
    }
    if git::config_get(bare, "forge.repo").await.as_deref() != Some(repo) {
        git::config_set(bare, "forge.repo", repo).await?;
        changed = true;
    }
    if git::config_get(bare, "forge.base-branch").await.as_deref() != Some(base_branch) {
        git::config_set(bare, "forge.base-branch", base_branch).await?;
        changed = true;
    }
    ensure_token(home)?;
    Ok(changed)
}

/// One project repository's guard, once installed (or found already
/// installed): what `forge init` and `forge project guard` both report.
pub struct GuardStep {
    pub repo: String,
    pub bare: PathBuf,
    pub changed: bool,
}

/// Install the guard into every repository of `project` whose push remote
/// is a bare repository on this machine; a repository with no such origin
/// (a network remote, or `push = false`) is silently skipped, the same as
/// `init::install_mirrors` skips one.
pub async fn install_for_project(
    home: &Path,
    store: &Store,
    project: &str,
) -> Result<Vec<GuardStep>> {
    let mut steps = Vec::new();
    for r in store.project_repos(project)? {
        let repo = Path::new(&r.repo);
        let Some(remote) = crate::init::origin_remote(repo) else {
            continue;
        };
        let base_branch = config::load_working(repo)
            .await
            .map(|c| c.base_branch)
            .unwrap_or_else(|_| "main".to_string());
        for bare in bare_destinations_sync(repo, &remote) {
            let changed = install(&bare, home, &r.repo, &base_branch).await?;
            steps.push(GuardStep {
                repo: r.repo.clone(),
                bare,
                changed,
            });
        }
    }
    Ok(steps)
}

/// Install the guard into every registered project's repositories: what
/// `forge init` does unconditionally, alongside `--mirror`.
pub async fn install_for_every_project(home: &Path, store: &Store) -> Result<Vec<GuardStep>> {
    let mut steps = Vec::new();
    for p in store.list_projects()? {
        steps.extend(install_for_project(home, store, &p.name).await?);
    }
    Ok(steps)
}

/// Local bare destinations of a remote, including every effective push URL
/// (Git applies pushInsteadOf here) and the fetch URL used by the integrator.
/// Shared by installation and synchronous doctor inspection.
pub fn bare_destinations_sync(repo: &Path, remote: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for args in [
        vec!["remote", "get-url", remote],
        vec!["remote", "get-url", "--push", "--all", remote],
    ] {
        let Ok(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        for url in String::from_utf8_lossy(&output.stdout).lines() {
            let Some(path) = git::local_remote_path(repo, url) else {
                continue;
            };
            if paths.contains(&path) {
                continue;
            }
            let bare = std::process::Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["rev-parse", "--is-bare-repository"])
                .output();
            if bare.is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true"
            }) {
                paths.push(path);
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_token_is_stable_600_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let first = ensure_token(&home).unwrap();
        assert_eq!(first.len(), 64);
        let second = ensure_token(&home).unwrap();
        assert_eq!(first, second);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(token_path(&home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn concurrent_installers_share_one_complete_token() {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(8);
        let tokens = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        ensure_token(dir.path()).unwrap()
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .collect::<Vec<_>>()
        });
        let saved = std::fs::read_to_string(token_path(dir.path())).unwrap();
        assert_eq!(saved.len(), 64);
        assert!(tokens.iter().all(|token| token == &saved));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn existing_token_permissions_are_repaired_without_rotating_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let token = ensure_token(dir.path()).unwrap();
        let path = token_path(dir.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(ensure_token(dir.path()).unwrap(), token);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn installed_is_false_until_the_hook_matches() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("origin.git");
        assert!(
            std::process::Command::new("git")
                .args(["init", "--bare"])
                .arg(&bare)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(!installed(&bare));
        std::fs::write(bare.join("hooks/pre-receive"), HOOK).unwrap();
        std::fs::set_permissions(
            bare.join("hooks/pre-receive"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(!installed(&bare));
        std::fs::set_permissions(
            bare.join("hooks/pre-receive"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(installed(&bare));
        std::fs::write(bare.join("hooks/pre-receive"), "#!/bin/sh\nexit 0\n").unwrap();
        assert!(!installed(&bare));
    }
}
