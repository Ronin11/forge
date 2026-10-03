//! Filesystem cleanup for verification overlays.

use std::path::{Path, PathBuf};

/// Where the overlay's manifest lives: inside the worktree's git directory,
/// so it is never part of the tree and survives a crash with it.
pub fn overlay_manifest_path(dest: &Path) -> PathBuf {
    dest.join(".git").join("forge-overlay")
}

/// Removes what an interrupted overlay left in `dest`, from the manifest
/// written before it was placed. Returns how many files were removed.
pub fn clear_recorded_overlay(worktree: &str) -> usize {
    if worktree.is_empty() {
        return 0;
    }
    let dest = Path::new(worktree);
    let path = overlay_manifest_path(dest);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let mut removed = 0;
    let mut dirs = Vec::new();
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("F", f)) if !f.split('/').any(|c| c == "..") && !f.starts_with('/') => {
                if std::fs::remove_file(dest.join(f)).is_ok() {
                    removed += 1;
                }
            }
            Some(("D", d)) if !d.split('/').any(|c| c == "..") && !d.starts_with('/') => {
                dirs.push(d.to_string())
            }
            _ => {}
        }
    }
    for d in dirs {
        let _ = remove_empty_dirs(&dest.join(d));
    }
    let _ = std::fs::remove_file(path);
    removed
}

pub fn remove_empty_dirs(dir: &Path) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            remove_empty_dirs(&p)?;
        }
    }
    if std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(())
}
