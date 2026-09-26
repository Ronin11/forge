//! Releases as directories (docs/OPS.md, "The running binary"):
//! `FORGE_HOME/bin/releases/<id>/` holds one release's binaries and
//! `FORGE_HOME/bin/current` is a symlink to the live one, flipped by an
//! atomic `rename` of a temporary symlink; `previous` keeps the last live
//! release. Nothing ever writes to a file a process is executing.

use anyhow::{Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The workspace's own release binaries, in the order `scripts/release.sh`
/// packs them (see `tests/release.rs`).
pub const BINS: &[&str] = &[
    "forge",
    "forge-web",
    "forge-portal",
    "forge-repomap",
    "forge-test",
    "forge-tui",
];

/// `FORGE_HOME/bin`, the directory holding `releases/`, `current` and `previous`.
pub fn root(home: &Path) -> PathBuf {
    home.join("bin")
}

pub fn release_dir(root: &Path, id: &str) -> PathBuf {
    root.join("releases").join(id)
}

/// The id a pointer (`current` or `previous`) names, if it exists.
pub fn pointed_at(root: &Path, pointer: &str) -> Option<String> {
    let target = std::fs::read_link(root.join(pointer)).ok()?;
    target.file_name()?.to_str().map(str::to_string)
}

/// Copy the binaries that exist in `src` into `releases/<id>/` (through a
/// temporary directory renamed into place), executable. `forge` itself is
/// required. Returns whether the release was created; an existing one is
/// never touched.
pub fn install(root: &Path, src: &Path, id: &str) -> Result<bool> {
    let dest = release_dir(root, id);
    if dest.exists() {
        return Ok(false);
    }
    anyhow::ensure!(
        src.join("forge").is_file(),
        "{} has no forge",
        src.display()
    );
    let tmp = root.join("releases").join(format!(".{id}.tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    for b in BINS {
        let from = src.join(b);
        if from.is_file() {
            let to = tmp.join(b);
            std::fs::copy(&from, &to).with_context(|| format!("copying {b} into {id}"))?;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    std::fs::rename(&tmp, &dest)?;
    Ok(true)
}

/// Make `<root>/<name>` a symlink to `releases/<id>`, atomically.
fn point(root: &Path, name: &str, id: &str) -> Result<()> {
    let tmp = root.join(format!(".{name}.new"));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(Path::new("releases").join(id), &tmp)?;
    std::fs::rename(&tmp, root.join(name))?;
    Ok(())
}

/// Flip `current` to `id` and move the old target to `previous`. Returns
/// the pointers as they were, for `restore`. A no-op when `id` is live.
pub fn flip(root: &Path, id: &str) -> Result<(Option<String>, Option<String>)> {
    anyhow::ensure!(
        release_dir(root, id).is_dir(),
        "no release {id} under {}",
        root.display()
    );
    let was = (pointed_at(root, "current"), pointed_at(root, "previous"));
    if was.0.as_deref() == Some(id) {
        return Ok(was);
    }
    if let Some(old) = &was.0 {
        point(root, "previous", old)?;
    }
    point(root, "current", id)?;
    Ok(was)
}

/// Put the pointers back as `flip` found them.
pub fn restore(root: &Path, was: &(Option<String>, Option<String>)) -> Result<()> {
    match &was.0 {
        Some(id) => point(root, "current", id)?,
        None => {
            let _ = std::fs::remove_file(root.join("current"));
        }
    }
    match &was.1 {
        Some(id) => point(root, "previous", id)?,
        None => {
            let _ = std::fs::remove_file(root.join("previous"));
        }
    }
    Ok(())
}

/// Make the symlink `link` point at `target`, replacing whatever is there;
/// false when it already did.
pub fn relink(link: &Path, target: &Path) -> Result<bool> {
    if std::fs::read_link(link).ok().as_deref() == Some(target) {
        return Ok(false);
    }
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = link.with_extension("forge-new");
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, link)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_src(dir: &Path, tag: &str) -> PathBuf {
        let src = dir.join(tag);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("forge"), tag).unwrap();
        src
    }

    #[test]
    fn flip_moves_the_old_target_to_previous_and_restore_undoes_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("bin");
        for id in ["a", "b"] {
            install(&root, &fake_src(dir.path(), id), id).unwrap();
        }
        let was = flip(&root, "a").unwrap();
        assert_eq!(was, (None, None));
        flip(&root, "b").unwrap();
        assert_eq!(pointed_at(&root, "current").as_deref(), Some("b"));
        assert_eq!(pointed_at(&root, "previous").as_deref(), Some("a"));
        assert_eq!(
            std::fs::read_to_string(root.join("current/forge")).unwrap(),
            "b"
        );
        let was = flip(&root, "b").unwrap();
        assert_eq!(was.0.as_deref(), Some("b"));
        restore(&root, &(Some("a".into()), None)).unwrap();
        assert_eq!(pointed_at(&root, "current").as_deref(), Some("a"));
        assert!(pointed_at(&root, "previous").is_none());
    }

    #[test]
    fn install_never_touches_an_existing_release() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("bin");
        assert!(install(&root, &fake_src(dir.path(), "x"), "r").unwrap());
        assert!(!install(&root, &fake_src(dir.path(), "y"), "r").unwrap());
        assert_eq!(
            std::fs::read_to_string(root.join("releases/r/forge")).unwrap(),
            "x"
        );
    }
}
