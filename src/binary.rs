use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Keep the launch name (including symlinks), which survives atomic replacement.
pub fn launch_path() -> Result<PathBuf> {
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
