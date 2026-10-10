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
                remove_tree(&path)?;
            } else {
                std::fs::remove_file(&path).map_err(|e| named(&path, e))?;
            }
        }
        bytes += count;
    }
    Ok(bytes)
}

/// Remove `path` with everything under it, like `std::fs::remove_dir_all`,
/// except that a directory nobody can read does not stop the removal.
/// overlayfs leaves its `work/work` directory mode 000: bubblewrap's
/// `--overlay` makes one per attempt under the worktree's overlay state
/// directory, and Forge's own e2e suite leaves thousands under a
/// worktree's `target/tmp`. `remove_dir_all` fails on the first one with a
/// bare "Permission denied" naming no path, which at a task's terminal
/// state write surfaced as an environment fault and exited the worker
/// (2026-10-04 to 10-10). Such directories are made readable first, and
/// every error names its path. A missing `path` is nothing to remove.
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    use std::io::ErrorKind::{NotFound, PermissionDenied};
    match std::fs::remove_dir_all(path) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == NotFound => return Ok(()),
        Err(e) if e.kind() == PermissionDenied => {}
        Err(e) => return Err(named(path, e)),
    }
    make_removable(path)?;
    std::fs::remove_dir_all(path).map_err(|e| named(path, e))
}

fn named(path: &Path, e: std::io::Error) -> std::io::Error {
    std::io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// Every directory under `path` (symlinks not followed) readable, writable
/// and searchable by its owner, so its entries can be listed and unlinked.
fn make_removable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = path.symlink_metadata().map_err(|e| named(path, e))?;
    if !meta.is_dir() {
        return Ok(());
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700))
            .map_err(|e| named(path, e))?;
    }
    for entry in std::fs::read_dir(path).map_err(|e| named(path, e))? {
        make_removable(&entry.map_err(|e| named(path, e))?.path())?;
    }
    Ok(())
}

/// Cleanup after a job executor returns, including early errors and skips.
pub struct JobCaches(pub PathBuf);
impl Drop for JobCaches {
    fn drop(&mut self) {
        if let Err(error) = caches(&self.0, false) {
            eprintln!("job cache cleanup {}: {error:#}", self.0.display());
        }
    }
}

/// `task_caches` for a task that just reached a terminal state: a cache
/// that still cannot be removed is reported on stderr and left for `forge
/// gc --caches`, never an error of the state change itself. A failed
/// removal there once became a bare environment fault that exited the
/// worker, killing every running attempt and plugin with it.
pub fn discard_task_caches(worktree: &str) {
    if let Err(e) = task_caches(worktree) {
        eprintln!("cache cleanup {worktree}: {e:#}");
    }
}

pub fn task_caches(worktree: &str) -> Result<u64> {
    if worktree.is_empty() {
        return Ok(0);
    }
    let mut bytes = caches(Path::new(worktree), false)?;
    for sibling in [
        crate::attempt::tests_clone_dir(worktree),
        crate::attempt::scratch_dir(worktree),
        PathBuf::from(format!("{worktree}-op")),
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
                PathBuf::from(format!("{tree}-op")),
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

    #[test]
    fn caches_removes_a_target_holding_unreadable_overlay_work_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let work = dir
            .path()
            .join("target/tmp/.tmpX/home/worktrees/2-overlays/.tmpY/work/work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(dir.path().join("target/data"), "x").unwrap();
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o000)).unwrap();
        // The plain removal is what used to run here, and what fails.
        assert!(std::fs::remove_dir_all(dir.path().join("target")).is_err());
        caches(dir.path(), false).unwrap();
        assert!(!dir.path().join("target").exists());
        // Nothing to remove is not an error; an unremovable file names itself.
        remove_tree(&dir.path().join("absent")).unwrap();
        discard_task_caches(dir.path().to_str().unwrap());
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::{
        ctx::Paths,
        store::{Task, TaskState},
    };

    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: dir.path().to_path_buf(),
            worktrees: dir.path().join("worktrees"),
            logs: dir.path().join("logs"),
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        (dir, Forge::open_with(paths, store).unwrap())
    }

    #[test]
    fn sweep_skips_running_tasks_and_includes_queued_and_terminal_trees() {
        let (_dir, f) = fixture();
        for (index, state) in [TaskState::Running, TaskState::Queued, TaskState::Failed]
            .into_iter()
            .enumerate()
        {
            let tree = f.paths.worktrees.join(index.to_string());
            let mut task = Task {
                worktree: tree.to_string_lossy().into_owned(),
                state,
                ..Default::default()
            };
            task.id = f.store.insert_task(&task).unwrap();
            f.store.update_task(&task).unwrap();
            std::fs::create_dir_all(tree.join("target")).unwrap();
            std::fs::write(tree.join("target/data"), vec![0; 8192]).unwrap();
            std::fs::write(tree.join("source"), "keep").unwrap();
        }
        let estimate = sweep(&f.store, &f.paths.worktrees, true).unwrap();
        assert!(estimate >= 16384);
        assert!(f.paths.worktrees.join("1/target/data").exists());
        assert_eq!(
            sweep(&f.store, &f.paths.worktrees, false).unwrap(),
            estimate
        );
        assert!(f.paths.worktrees.join("0/target/data").exists());
        for id in [1, 2] {
            assert!(!f.paths.worktrees.join(format!("{id}/target")).exists());
            assert!(f.paths.worktrees.join(format!("{id}/source")).exists());
        }
    }

    #[test]
    fn sweep_includes_finished_jobs_but_preserves_running_jobs() {
        use crate::store::{Job, JobState};
        let (_dir, f) = fixture();
        f.store
            .create_project(&crate::store::Project {
                name: "cache-test".into(),
                ..Default::default()
            })
            .unwrap();
        for state in [JobState::Running, JobState::Failed] {
            let id = f
                .store
                .create_job(&Job {
                    project: "cache-test".into(),
                    state,
                    ..Default::default()
                })
                .unwrap();
            let target = f.paths.worktrees.join(format!("job-{id}/target"));
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("data"), vec![1; 8192]).unwrap();
        }
        assert!(sweep(&f.store, &f.paths.worktrees, false).unwrap() >= 8192);
        assert!(f.paths.worktrees.join("job-1/target/data").exists());
        assert!(!f.paths.worktrees.join("job-2/target").exists());
    }

    #[test]
    fn terminal_states_remove_caches_immediately() {
        let (_dir, f) = fixture();
        for state in [
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::Unverified,
            TaskState::Blocked,
            TaskState::Withdrawn,
            TaskState::Capped,
        ] {
            let tree = f.paths.worktrees.join(state.as_str());
            std::fs::create_dir_all(tree.join("target")).unwrap();
            let mut task = Task {
                worktree: tree.to_string_lossy().into_owned(),
                state: TaskState::Running,
                ..Default::default()
            };
            task.id = f.store.insert_task(&task).unwrap();
            task.state = state;
            f.store.update_task(&task).unwrap();
            assert!(!tree.join("target").exists(), "{}", state.as_str());
        }
    }

    #[test]
    fn disk_hold_announces_once_until_space_recovers() {
        let (_dir, mut f) = fixture();
        f.worker.min_free_gb = u64::MAX;
        assert!(check_claim(&f).unwrap());
        assert!(check_claim(&f).unwrap());
        let events = f.paths.home.join("events.jsonl");
        let count = || {
            std::fs::read_to_string(&events)
                .unwrap()
                .matches("disk_held")
                .count()
        };
        assert_eq!(count(), 1);
        let text = std::fs::read_to_string(&events).unwrap();
        assert!(text.contains("forge gc --caches would free"));
        f.worker.min_free_gb = 0;
        assert!(!check_claim(&f).unwrap());
        f.worker.min_free_gb = u64::MAX;
        assert!(check_claim(&f).unwrap());
        assert_eq!(count(), 2);
    }
}
