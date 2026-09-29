//! Disk accounting and disposable worktree build caches.
use crate::{ctx::Forge, store::Store};
use anyhow::Result;
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const CACHES: &[&str] = &["target", "node_modules/.cache", ".godot"];
const GB: u64 = 1024 * 1024 * 1024;

pub fn free_bytes(path: &Path) -> Result<u64> {
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL terminated and statvfs initializes stats on success.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stats = unsafe { stats.assume_init() };
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

pub fn holds(free: u64, min_free_gb: u64) -> bool {
    free < min_free_gb.saturating_mul(GB)
}

/// Allocated bytes, without traversing symlinks outside the worktree.
pub fn size(path: &Path) -> u64 {
    let Ok(meta) = path.symlink_metadata() else {
        return 0;
    };
    let own = meta.blocks().saturating_mul(512);
    if !meta.is_dir() {
        return own;
    }
    own.saturating_add(
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| size(&entry.path()))
            .sum::<u64>(),
    )
}

pub fn caches(worktree: &Path, dry_run: bool) -> Result<u64> {
    if !worktree.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return Ok(0);
    }
    let mut bytes = 0;
    for relative in CACHES {
        let path = worktree.join(relative);
        // In particular, never follow a node_modules symlink into a shared cache.
        if path
            .parent()
            .is_some_and(|p| p != worktree && !p.symlink_metadata().is_ok_and(|m| m.is_dir()))
        {
            continue;
        }
        let meta = match path.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let count = size(&path);
        if !dry_run {
            if meta.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
        }
        bytes += count;
    }
    Ok(bytes)
}

pub fn task_caches(worktree: &str) -> Result<u64> {
    if worktree.is_empty() {
        return Ok(0);
    }
    let mut bytes = caches(Path::new(worktree), false)?;
    for sibling in [
        crate::attempt::tests_clone_dir(worktree),
        crate::attempt::scratch_dir(worktree),
    ] {
        bytes += caches(&sibling, false)?;
    }
    Ok(bytes)
}

/// Keep the claim transaction excluded throughout deletion: a queued task
/// must not start building while gc removes its cache.
pub fn sweep(store: &Store, root: &Path, dry_run: bool) -> Result<u64> {
    let mut conn = store.lock();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut trees = Vec::new();
    let mut running = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT worktree, state FROM tasks WHERE worktree != ''")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (tree, state) = row?;
            let paths = [
                PathBuf::from(&tree),
                crate::attempt::tests_clone_dir(&tree),
                crate::attempt::scratch_dir(&tree),
            ];
            if state == "running" {
                running.extend(paths);
            } else {
                trees.extend(paths);
            }
        }
        let mut stmt = tx.prepare("SELECT id, state FROM jobs")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, state) = row?;
            let tree = root.join(format!("job-{id}"));
            if state == "running" {
                running.push(tree);
            } else {
                trees.push(tree);
            }
        }
    }
    trees.sort();
    trees.dedup();
    let mut bytes = 0;
    for tree in trees {
        if !running.contains(&tree) {
            bytes += caches(&tree, dry_run)?;
        }
    }
    tx.commit()?;
    Ok(bytes)
}

pub fn worktrees(root: &Path) -> String {
    let mut sizes: Vec<_> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            (
                size(&e.path()),
                e.file_name().to_string_lossy().into_owned(),
            )
        })
        .collect();
    let total: u64 = sizes.iter().map(|(size, _)| size).sum();
    sizes.sort_by(|a, b| b.cmp(a));
    let largest = sizes
        .iter()
        .take(10)
        .map(|(n, name)| format!("{name}: {n} bytes"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("total {total} bytes; ten largest: {largest}")
}

/// A marker shared by workers deduplicates notifications across restarts.
pub fn check_claim(f: &Forge) -> Result<bool> {
    let free = free_bytes(&f.paths.home)?;
    let held = holds(free, f.worker.min_free_gb);
    let marker = f.paths.home.join("disk-held");
    if !held {
        match std::fs::remove_file(marker) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        return Ok(false);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(_) => {
            let reclaimable = sweep(&f.store, &f.paths.worktrees, true)?;
            let reason = format!(
                "{}: {free} bytes free, below {} GiB; claims held; forge gc --caches would free {reclaimable} bytes",
                f.paths.home.display(),
                f.worker.min_free_gb
            );
            eprintln!("{reason}");
            f.report.emit(
                0,
                crate::report::Event::DiskHeld {
                    reason: &reason,
                    audience: "person",
                },
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_hold_boundary_and_disabled_threshold() {
        assert!(holds(50 * GB - 1, 50));
        assert!(!holds(50 * GB, 50));
        assert!(!holds(0, 0));
        assert!(free_bytes(Path::new(".")).unwrap() > 0);
        assert!(free_bytes(Path::new("/nonexistent-forge-disk-test")).is_err());
    }

    #[test]
    fn cache_sweep_preserves_sources_and_symlink_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("keep"), "data").unwrap();
        for cache in CACHES {
            let p = dir.path().join(cache);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("data"), vec![1; 8192]).unwrap();
        }
        std::fs::write(dir.path().join("source"), "keep").unwrap();
        let expected = caches(dir.path(), true).unwrap();
        assert!(expected >= 3 * 8192);
        assert!(dir.path().join("target/data").exists());
        assert_eq!(caches(dir.path(), false).unwrap(), expected);
        assert!(dir.path().join("source").exists());
        assert_eq!(caches(dir.path(), false).unwrap(), 0);
        std::os::unix::fs::symlink(external.path(), dir.path().join("target")).unwrap();
        caches(dir.path(), false).unwrap();
        assert!(external.path().join("keep").exists());
        std::fs::remove_dir(dir.path().join("node_modules")).unwrap();
        std::os::unix::fs::symlink(external.path(), dir.path().join("node_modules")).unwrap();
        std::fs::create_dir(external.path().join(".cache")).unwrap();
        caches(dir.path(), false).unwrap();
        assert!(external.path().join(".cache").exists());
    }
}
