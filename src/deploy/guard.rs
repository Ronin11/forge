//! The `deploy-self` guard against staging or going live on a release
//! older than one already there, and against racing another deploy to the
//! lock the `deploy-self` script itself takes (docs/REVIEW-4.md, E3-11).
//!
//! The three holes this closes: (a) any git error, or a live or staged id
//! that is not a commit the kernel repository holds (`forge upgrade`
//! names a release `0.4.0`; a `--relink` id may be a short sha the kernel
//! never fetched), used to skip the guard silently instead of refusing;
//! (b) it compared only with `current`, not with `staged`, so a deploy of
//! an older commit could pass between a stage and the successor's flip;
//! (c) it ran before `bin/.deploy-self.lock`, which is not fair, so of two
//! deploys racing for the lock the older commit could still stage last.

use crate::git;
use crate::release;
use anyhow::{Result, bail};
use std::path::Path;

/// Take `bin/.deploy-self.lock`, the file `deploy-self.toml`'s script
/// locks too, for the span of a self-deploy's `origin_truth` and its
/// method: both must run under the one lock so that of two deploys
/// racing for it, [`not_older_than_live`] never runs for one before the
/// other has staged (docs/REVIEW-4.md, E3-11c). The script is told with
/// `FORGE_DEPLOY_LOCK_HELD=1` to skip its own `flock`, which would
/// otherwise wait on the very process holding this lock.
pub(crate) fn take_lock(home: &Path) -> Result<release::Lock> {
    release::lock(&release::root(home))
}

/// The newer of `current` and `staged`, resolved to a commit in `kernel`,
/// with the pointer's name for messages. `None` when neither pointer
/// exists. An existing pointer whose id does not resolve to a commit
/// `kernel` holds is an error, never silently skipped.
async fn newest_release(kernel: &Path, root: &Path) -> Result<Option<(&'static str, String)>> {
    let mut newest: Option<(&'static str, String)> = None;
    for (pointer, label) in [("current", "live"), ("staged", "staged")] {
        let Some(id) = release::pointed_at(root, pointer) else {
            continue;
        };
        let commit = git::rev_parse(kernel, &format!("{id}^{{commit}}"))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "the {label} release is {id}, which is not a commit {} holds; whether a \
                     deploy is older than it cannot be checked, so it is refused",
                    kernel.display()
                )
            })?;
        newest = Some(match newest {
            None => (label, commit),
            Some((prev_label, prev_commit)) => {
                if git::is_ancestor(kernel, &prev_commit, &commit).await {
                    (label, commit)
                } else {
                    (prev_label, prev_commit)
                }
            }
        });
    }
    Ok(newest)
}

/// Refuse `sha` when it is an ancestor of the newer of the live release
/// (`current`) and the one already staged (`staged`): migrations do not
/// run backwards. Fails closed rather than open: a live or staged id that
/// does not resolve to a commit `kernel` holds is refused too, unless
/// `force`, which skips the check entirely, the way it always has.
pub(crate) async fn not_older_than_live(
    kernel: &Path,
    home: &Path,
    sha: &str,
    force: bool,
) -> Result<()> {
    if force {
        return Ok(());
    }
    let root = release::root(home);
    let Some((label, reference)) = newest_release(kernel, &root).await? else {
        return Ok(());
    };
    if reference != sha && git::is_ancestor(kernel, sha, &reference).await {
        bail!(
            "{} is older than the {label} release {} (an ancestor of it); migrations do not \
             run backwards, so it is refused. Pass --force to deploy it anyway.",
            super::short(sha),
            super::short(&reference)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(root: &Path, pointer: &str, id: &str) {
        std::fs::create_dir_all(root).unwrap();
        let link = root.join(pointer);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(format!("releases/{id}"), link).unwrap();
    }

    async fn commit(repo: &Path, flag: &str) -> String {
        std::fs::write(repo.join("flag.txt"), flag).unwrap();
        git::commit_all(repo, flag).await.unwrap().unwrap()
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(dir.path())
                .status()
                .unwrap()
                .success()
        );
        dir
    }

    #[tokio::test]
    async fn a_commit_older_than_only_staged_is_refused() {
        let repo = init_repo();
        let kernel = repo.path();
        let base = commit(kernel, "base").await;
        let descendant = commit(kernel, "descendant").await;
        let home = tempfile::tempdir().unwrap();
        let root = release::root(home.path());
        point(&root, "current", &base);
        point(&root, "staged", &descendant);

        let err = not_older_than_live(kernel, home.path(), &base, false)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&base[..8]), "{msg}");
        assert!(msg.contains(&descendant[..8]), "{msg}");
        assert!(msg.contains("staged"), "{msg}");
        assert!(msg.contains("--force"), "{msg}");

        // The descendant itself, and anything with --force, still deploy.
        not_older_than_live(kernel, home.path(), &descendant, false)
            .await
            .unwrap();
        not_older_than_live(kernel, home.path(), &base, true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_live_id_that_is_not_a_commit_refuses_rather_than_skips() {
        let repo = init_repo();
        let kernel = repo.path();
        let sha = commit(kernel, "only").await;
        let home = tempfile::tempdir().unwrap();
        let root = release::root(home.path());
        // `forge upgrade` names its release by version, not a sha the
        // kernel repository has ever heard of.
        point(&root, "current", "0.4.0");

        let err = not_older_than_live(kernel, home.path(), &sha, false)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("0.4.0"), "{msg}");
        assert!(msg.contains("not a commit"), "{msg}");

        // --force skips the check outright rather than resolving it.
        not_older_than_live(kernel, home.path(), &sha, true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn no_pointers_at_all_is_not_a_refusal() {
        let repo = init_repo();
        let kernel = repo.path();
        let sha = commit(kernel, "only").await;
        let home = tempfile::tempdir().unwrap();
        not_older_than_live(kernel, home.path(), &sha, false)
            .await
            .unwrap();
    }
}
