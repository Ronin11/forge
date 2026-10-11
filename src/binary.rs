use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Keep the launch name (including symlinks), which survives atomic
/// replacement; a path inside `<bin>/releases/<id>/` is named through
/// `<bin>/current` instead, the one path that stays valid across a flip.
pub fn launch_path() -> Result<PathBuf> {
    Ok(through_current(absolute_launch_path()?))
}

/// The forge a plugin or a job runs: `<home>/bin/current/forge` whenever
/// the release layout has one, never a launch path that a rebuilt or
/// deleted checkout can pull out from under them (a plugin once
/// crash-looped on a removed `target/release/forge`); the launch path
/// only where there is no release layout at all.
pub fn for_home(home: &Path) -> Result<PathBuf> {
    let current = home.join("bin").join("current").join("forge");
    if current.is_file() {
        return Ok(current);
    }
    launch_path()
}

/// `<bin>/releases/<id>/<name>` -> `<bin>/current/<name>` when `<bin>/current` exists.
pub fn through_current(path: PathBuf) -> PathBuf {
    let Some(name) = path.file_name() else {
        return path;
    };
    let release = path.parent();
    let releases = release.and_then(Path::parent);
    let bin = releases.and_then(Path::parent);
    match (releases.and_then(Path::file_name), bin) {
        (Some(dir), Some(bin)) if dir == "releases" && bin.join("current").exists() => {
            bin.join("current").join(name)
        }
        _ => path,
    }
}

fn absolute_launch_path() -> Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os("FORGE_BIN")
            .or_else(|| std::env::args_os().next())
            .context("the forge binary's launch path")?,
    );
    if path.is_absolute() {
        return Ok(path);
    }
    if path.components().count() == 1
        && let Some(found) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(&path))
                .find(|candidate| candidate.is_file())
        })
    {
        return Ok(std::env::current_dir()?.join(found));
    }
    Ok(std::env::current_dir()?.join(path))
}

/// Linux appends this marker to /proc/self/exe after an atomic replacement.
/// Unit installation needs the canonical binary directory for sibling programs,
/// but must write the replacement's usable filename, not the deleted inode name.
pub fn without_deleted_suffix(path: &Path) -> PathBuf {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let bytes = path.as_os_str().as_bytes();
    std::ffi::OsString::from_vec(bytes.strip_suffix(b" (deleted)").unwrap_or(bytes).to_vec()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_path_is_named_through_current() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(bin.join("releases/abc")).unwrap();
        let path = bin.join("releases/abc/forge");
        assert_eq!(through_current(path.clone()), path);
        std::os::unix::fs::symlink("releases/abc", bin.join("current")).unwrap();
        assert_eq!(through_current(path), bin.join("current/forge"));
        assert_eq!(
            through_current(PathBuf::from("/x/forge")),
            Path::new("/x/forge")
        );
    }

    #[test]
    fn plugins_and_jobs_run_the_homes_current_forge_when_there_is_one() {
        let dir = tempfile::tempdir().unwrap();
        assert_ne!(
            for_home(dir.path()).unwrap(),
            dir.path().join("bin/current/forge")
        );
        std::fs::create_dir_all(dir.path().join("bin/releases/abc")).unwrap();
        std::fs::write(dir.path().join("bin/releases/abc/forge"), "").unwrap();
        std::os::unix::fs::symlink("releases/abc", dir.path().join("bin/current")).unwrap();
        assert_eq!(
            for_home(dir.path()).unwrap(),
            dir.path().join("bin/current/forge")
        );
    }

    #[test]
    fn removes_only_a_trailing_deleted_suffix() {
        assert_eq!(
            without_deleted_suffix(Path::new("/tmp/forge (deleted)")),
            Path::new("/tmp/forge")
        );
        for path in ["/tmp/forge", "/tmp/forge (deleted)/forge"] {
            assert_eq!(without_deleted_suffix(Path::new(path)), Path::new(path));
        }
    }
}
