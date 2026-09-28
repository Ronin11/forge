//! Releases as directories (docs/OPS.md, "The running binary"):
//! `FORGE_HOME/bin/releases/<id>/` holds one release's binaries and
//! `FORGE_HOME/bin/current` is a symlink to the live one, flipped by an
//! atomic `rename` of a temporary symlink; `previous` keeps the last live
//! release. Nothing ever writes to a file a process is executing.

use anyhow::{Context, Result};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A private staging path for one operation, including concurrent calls in one process.
pub(crate) fn temporary_path(parent: &Path, name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    parent.join(format!(
        ".{name}.tmp.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

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

/// How long `lock` waits for another writer of the pointers, as long as
/// `deploy-self` waits for its own lock.
const LOCK_WAIT: Duration = Duration::from_secs(600);

/// The one lock every writer of `current`, `previous` and `releases/`
/// holds: `flock` on `bin/.deploy-self.lock`, the file the `deploy-self`
/// script locks too. Released when dropped, or when the process dies.
#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] std::fs::File);

/// Take the pointers' lock, waiting for whoever holds it.
pub fn lock(root: &Path) -> Result<Lock> {
    lock_within(root, LOCK_WAIT)
}

fn lock_within(root: &Path, wait: Duration) -> Result<Lock> {
    std::fs::create_dir_all(root.join("releases"))?;
    let path = root.join(".deploy-self.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let start = Instant::now();
    loop {
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Lock(file));
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock {
            return Err(err).with_context(|| format!("locking {}", path.display()));
        }
        anyhow::ensure!(
            start.elapsed() < wait,
            "{} was held for {} s by another release writer",
            path.display(),
            wait.as_secs()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn release_dir(root: &Path, id: &str) -> PathBuf {
    root.join("releases").join(id)
}

/// The id a pointer (`current` or `previous`) names, if it exists.
pub fn pointed_at(root: &Path, pointer: &str) -> Option<String> {
    let target = std::fs::read_link(root.join(pointer)).ok()?;
    target.file_name()?.to_str().map(str::to_string)
}

/// The release this process runs: what its launcher said (`FORGE_RELEASE`),
/// else the `releases/<id>` its executable sits in, else what `current`
/// names, else "" for a binary that lives outside the layout.
pub fn running(root: &Path) -> String {
    if let Some(id) = std::env::var("FORGE_RELEASE")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return id;
    }
    let from_exe = std::env::current_exe().ok().and_then(|exe| {
        let release = exe.parent()?;
        (release.parent()?.file_name()? == "releases")
            .then(|| release.file_name()?.to_str().map(str::to_string))?
    });
    from_exe
        .or_else(|| pointed_at(root, "current"))
        .unwrap_or_default()
}

/// Copy the binaries that exist in `src` into `releases/<id>/` (through a
/// temporary directory renamed into place), executable. `forge` itself is
/// required. Returns whether the release was created; an existing one is
/// never touched.
pub fn install(_lock: &Lock, root: &Path, src: &Path, id: &str) -> Result<bool> {
    let dest = release_dir(root, id);
    if dest.exists() {
        return Ok(false);
    }
    anyhow::ensure!(
        src.join("forge").is_file(),
        "{} has no forge",
        src.display()
    );
    let tmp = temporary_path(&root.join("releases"), id);
    std::fs::create_dir(&tmp)?;
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

/// Make `<root>/<name>` a symlink to `releases/<id>`, atomically. The
/// temporary name carries the pid and a per-process counter.
fn point(_lock: &Lock, root: &Path, name: &str, id: &str) -> Result<()> {
    let tmp = temporary_path(root, name);
    std::os::unix::fs::symlink(Path::new("releases").join(id), &tmp)?;
    std::fs::rename(&tmp, root.join(name))?;
    Ok(())
}

/// Flip `current` to `id` and move the old target to `previous`. Returns
/// the pointers as they were, for `restore`. A no-op when `id` is live.
pub fn flip(lock: &Lock, root: &Path, id: &str) -> Result<(Option<String>, Option<String>)> {
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
        point(lock, root, "previous", old)?;
    }
    point(lock, root, "current", id)?;
    Ok(was)
}

/// Put the pointers back as `flip` found them.
pub fn restore(lock: &Lock, root: &Path, was: &(Option<String>, Option<String>)) -> Result<()> {
    match &was.0 {
        Some(id) => point(lock, root, "current", id)?,
        None => {
            let _ = std::fs::remove_file(root.join("current"));
        }
    }
    match &was.1 {
        Some(id) => point(lock, root, "previous", id)?,
        None => {
            let _ = std::fs::remove_file(root.join("previous"));
        }
    }
    Ok(())
}

/// Put `staged` back to what `current` names (gone when `current` names
/// nothing), but only while it still names `id`, the release a deploy
/// staged: a later deploy's stage is left alone. Returns whether it moved.
pub fn unstage(lock: &Lock, root: &Path, id: &str) -> Result<bool> {
    if pointed_at(root, "staged").as_deref() != Some(id) {
        return Ok(false);
    }
    match pointed_at(root, "current") {
        Some(live) => point(lock, root, "staged", &live)?,
        None => std::fs::remove_file(root.join("staged"))?,
    }
    Ok(true)
}

/// Withdraw the stage request whatever it names, as every flip of `current`
/// other than `deploy-self`'s does first: a flip decides what runs, and a
/// `staged` left behind would start a successor that flips it back.
/// Returns what `staged` named.
pub fn drop_staged(_lock: &Lock, root: &Path) -> Result<Option<String>> {
    let named = pointed_at(root, "staged");
    match std::fs::remove_file(root.join("staged")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(named),
    }
}

/// Acknowledge the stage request for `id`, the release that now runs:
/// remove `staged` only while it still names `id`, so a later deploy's
/// stage is left alone. Returns whether it was removed.
pub fn acknowledge_staged(_lock: &Lock, root: &Path, id: &str) -> Result<bool> {
    if pointed_at(root, "staged").as_deref() != Some(id) {
        return Ok(false);
    }
    std::fs::remove_file(root.join("staged"))?;
    Ok(true)
}

/// Whether `staged` is older than `current` in `previous`'s lineage: it
/// names `previous`, or `current` was pointed somewhere after it was
/// staged (an upgrade, a hand flip back to `previous`). Such a stage was
/// overtaken and must not start a successor that flips `current` back.
pub fn staged_overtaken(root: &Path) -> bool {
    let Some(staged) = pointed_at(root, "staged") else {
        return false;
    };
    if pointed_at(root, "current").as_deref() == Some(staged.as_str()) {
        return false;
    }
    if pointed_at(root, "previous").as_deref() == Some(staged.as_str()) {
        return true;
    }
    let pointed = |name: &str| {
        std::fs::symlink_metadata(root.join(name))
            .and_then(|m| m.modified())
            .ok()
    };
    matches!((pointed("staged"), pointed("current")), (Some(s), Some(c)) if s < c)
}

/// Take the pointers' lock only if nobody holds it: for a worker's tick,
/// which must not wait out a `deploy-self` build.
pub fn try_lock(root: &Path) -> Option<Lock> {
    lock_within(root, Duration::ZERO).ok()
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
        let lock = lock(&root).unwrap();
        for id in ["a", "b"] {
            install(&lock, &root, &fake_src(dir.path(), id), id).unwrap();
        }
        let was = flip(&lock, &root, "a").unwrap();
        assert_eq!(was, (None, None));
        flip(&lock, &root, "b").unwrap();
        assert_eq!(pointed_at(&root, "current").as_deref(), Some("b"));
        assert_eq!(pointed_at(&root, "previous").as_deref(), Some("a"));
        assert_eq!(
            std::fs::read_to_string(root.join("current/forge")).unwrap(),
            "b"
        );
        let was = flip(&lock, &root, "b").unwrap();
        assert_eq!(was.0.as_deref(), Some("b"));
        restore(&lock, &root, &(Some("a".into()), None)).unwrap();
        assert_eq!(pointed_at(&root, "current").as_deref(), Some("a"));
        assert!(pointed_at(&root, "previous").is_none());
    }

    #[test]
    fn install_never_touches_an_existing_release() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("bin");
        let lock = lock(&root).unwrap();
        assert!(install(&lock, &root, &fake_src(dir.path(), "x"), "r").unwrap());
        assert!(!install(&lock, &root, &fake_src(dir.path(), "y"), "r").unwrap());
        assert_eq!(
            std::fs::read_to_string(root.join("releases/r/forge")).unwrap(),
            "x"
        );
    }

    #[test]
    fn two_threads_flipping_alternately_never_leave_previous_naming_current() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("bin");
        {
            let lock = lock(&root).unwrap();
            for id in ["a", "b", "c"] {
                install(&lock, &root, &fake_src(dir.path(), id), id).unwrap();
            }
        }
        let threads: Vec<_> = [["a", "b"], ["b", "c"]]
            .into_iter()
            .map(|ids| {
                let root = root.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        let lock = lock(&root).unwrap();
                        flip(&lock, &root, ids[i % 2]).unwrap();
                        let current = pointed_at(&root, "current");
                        let previous = pointed_at(&root, "previous");
                        assert!(
                            previous.is_none() || previous != current,
                            "previous {previous:?} names what current names"
                        );
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn a_staged_release_is_overtaken_by_a_later_flip_or_by_naming_previous() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("bin");
        let lock = lock(&root).unwrap();
        for id in ["a", "b", "c"] {
            install(&lock, &root, &fake_src(dir.path(), id), id).unwrap();
        }
        flip(&lock, &root, "a").unwrap();
        assert!(!staged_overtaken(&root), "nothing staged");
        std::thread::sleep(Duration::from_millis(20));
        point(&lock, &root, "staged", "b").unwrap();
        assert!(!staged_overtaken(&root), "a fresh stage is a request");
        std::thread::sleep(Duration::from_millis(20));
        // An upgrade (or a hand flip) after the stage overtakes it.
        flip(&lock, &root, "c").unwrap();
        assert!(staged_overtaken(&root));
        // Staged naming previous is older than current.
        point(&lock, &root, "staged", "a").unwrap();
        assert!(staged_overtaken(&root));
        // Acknowledged only while it names the release that runs.
        assert!(!acknowledge_staged(&lock, &root, "c").unwrap());
        assert!(acknowledge_staged(&lock, &root, "a").unwrap());
        assert!(pointed_at(&root, "staged").is_none());
        point(&lock, &root, "staged", "b").unwrap();
        assert_eq!(drop_staged(&lock, &root).unwrap().as_deref(), Some("b"));
        assert_eq!(drop_staged(&lock, &root).unwrap(), None);
    }

    #[test]
    fn the_lock_waits_for_its_holder_and_gives_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("bin");
        let held = lock(&root).unwrap();
        assert!(lock_within(&root, Duration::from_millis(100)).is_err());
        drop(held);
        assert!(lock_within(&root, Duration::from_millis(100)).is_ok());
    }
}

#[cfg(test)]
mod temporary_tests {
    #[test]
    fn temporary_names_are_unique_across_threads() {
        let threads: Vec<_> = (0..16)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..100)
                        .map(|_| super::temporary_path(std::path::Path::new("/tmp"), "current"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let paths: std::collections::HashSet<_> = threads
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();
        assert_eq!(paths.len(), 1600);
        for path in paths {
            assert!(
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains(&format!(".tmp.{}.", std::process::id()))
            );
        }
    }
}
