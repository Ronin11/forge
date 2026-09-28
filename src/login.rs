//! The agent login belongs to the kernel, not to any one sandbox.
//!
//! The claude CLI keeps its OAuth pair in `<config dir>/.credentials.json`.
//! Each sandbox gets a private copy seeded from that host file (see
//! `sandbox::Sandbox::command`), and the refresh token rotates: a sandbox
//! that refreshes holds the only live pair, and the host file's refresh token
//! is dead from then on. The next host-side refresh fails and the CLI empties
//! the file (2026-09-27: 72 attempts died on 'OAuth session expired and could
//! not be refreshed'). So the kernel copies a later private pair back over
//! the host file, atomically and under a lock; refreshes a token near expiry
//! on the host before a launch; and never seeds an empty file.

use serde_json::Value;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The CLI's credentials file, in its config directory.
pub const FILE: &str = ".credentials.json";

/// Held while the host file is read, replaced or seeded from.
const LOCK: &str = ".forge-credentials.lock";

/// Unix seconds of the last write-back, for `forge doctor`.
const MARK: &str = ".forge-writeback";

/// A token this close to expiring is refreshed on the host before a launch,
/// at the least (see `refresh_window_ms`).
pub const REFRESH_WINDOW_MS: i64 = 30 * 60 * 1000;

/// Slack past a launch's timeout and its checks' before its token may expire.
const REFRESH_SLACK_MS: i64 = 5 * 60 * 1000;

/// How close to expiring a token is refreshed on the host before a launch
/// that may run for `timeout` and then its checks for `check_timeout`: long
/// enough that the token cannot expire under the attempt, where concurrent
/// attempts seeded with the same pair would all refresh with the one
/// rotating refresh token and all but one lose it.
pub fn refresh_window_ms(timeout: std::time::Duration, check_timeout: std::time::Duration) -> i64 {
    let ms = |d: std::time::Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    ms(timeout)
        .saturating_add(ms(check_timeout))
        .saturating_add(REFRESH_SLACK_MS)
        .max(REFRESH_WINDOW_MS)
}

/// How far back `forge doctor` looks for a write-back.
pub const WRITE_BACK_WINDOW_SECS: i64 = 8 * 3600;

/// What a credentials file says, as far as the kernel cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Creds {
    /// Both tokens present and non-empty.
    pub usable: bool,
    /// `expiresAt` in unix milliseconds; 0 when absent.
    pub expires_at_ms: i64,
}

impl Creds {
    /// Nothing there: no file, or one that says nothing.
    pub const NONE: Creds = Creds {
        usable: false,
        expires_at_ms: 0,
    };

    /// Read a credentials file's text. Never fails: text that is not the
    /// CLI's JSON is an unusable login that expires at 0.
    pub fn parse(text: &str) -> Creds {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Creds::NONE;
        };
        let o = &v["claudeAiOauth"];
        let token = |k: &str| o[k].as_str().is_some_and(|s| !s.trim().is_empty());
        // Milliseconds, as the CLI writes them; a bare seconds stamp is
        // read as such rather than as 1970.
        let raw = o["expiresAt"].as_i64().unwrap_or(0).max(0);
        Creds {
            usable: token("accessToken") && token("refreshToken"),
            expires_at_ms: if raw < 100_000_000_000 {
                raw * 1000
            } else {
                raw
            },
        }
    }

    /// Whether a launch should refresh this login on the host first, given
    /// its refresh window (`refresh_window_ms`).
    pub fn near_expiry(&self, now_ms: i64, window_ms: i64) -> bool {
        self.expires_at_ms < now_ms.saturating_add(window_ms)
    }
}

/// The host's login as a launch sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// No credentials file: an API key, or no login at all. Nothing to seed.
    Missing,
    /// A file with an empty or missing token, or no JSON at all.
    Empty,
    Usable(Creds),
}

pub fn host_state(dir: &Path) -> Host {
    match std::fs::read_to_string(dir.join(FILE)) {
        Err(_) => Host::Missing,
        Ok(t) => match Creds::parse(&t) {
            c if c.usable => Host::Usable(c),
            _ => Host::Empty,
        },
    }
}

/// The claude CLI's config directory: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
}

/// Whether a private copy's pair replaces the host file's: it is a whole
/// login that expires later. An unusable host file takes any private pair
/// that has not already expired, and never one that has (an expired copy
/// restored over an empty file would only hide that the login is gone).
pub fn should_write_back(host: Creds, private: Creds, now_ms: i64) -> bool {
    private.usable
        && private.expires_at_ms > host.expires_at_ms
        && (host.usable || private.expires_at_ms > now_ms)
}

/// Replace `dest` with `bytes` the way the CLI does: write a sibling, then
/// rename it over, so a reader sees the old file or the new one, never half.
pub fn replace_atomic(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = dest
        .file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let tmp = dest.with_file_name(format!(
        "{name}.forge-{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, dest)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Whether `path` is itself a regular file: a symlink (which a sandbox can
/// plant in a directory it writes) is not, whatever it points at.
pub fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Seed `dest`, a path the sandbox can write, with the bytes of the host
/// file `from`. The bytes go to a fresh sibling that is renamed over `dest`:
/// the rename replaces a planted symlink and never follows it, where
/// `std::fs::copy` would open the symlink's target and truncate it.
pub fn seed_copy(from: &Path, dest: &Path) -> std::io::Result<()> {
    replace_atomic(dest, &std::fs::read(from)?)
}

/// An exclusive lock on the host login, released on drop. `flock` locks the
/// open file, so it holds against another thread of this process as well as
/// against another process.
pub struct Lock(#[allow(dead_code)] Option<std::fs::File>);

/// Wait for the lock on `dir`'s login. A directory that cannot be locked (it
/// does not exist, so there is no login to race over) locks nothing.
pub fn lock(dir: &Path) -> Lock {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))
        .ok();
    if let Some(f) = &file {
        // SAFETY: the descriptor is open for the life of `f`.
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
    }
    Lock(file)
}

fn unix_ms() -> i64 {
    crate::unix_now() * 1000
}

/// `write_back` for a caller that holds the lock.
pub fn write_back_locked(dir: &Path, private: &Path) -> std::io::Result<bool> {
    let host = dir.join(FILE);
    let Ok(host_text) = std::fs::read_to_string(&host) else {
        // Logged out (or never in): a stale copy does not undo that.
        return Ok(false);
    };
    if !is_regular_file(private) {
        return Ok(false);
    }
    let Ok(bytes) = std::fs::read(private) else {
        return Ok(false);
    };
    let theirs = Creds::parse(&String::from_utf8_lossy(&bytes));
    if !should_write_back(Creds::parse(&host_text), theirs, unix_ms()) {
        return Ok(false);
    }
    replace_atomic(&host, &bytes)?;
    let _ = replace_atomic(&dir.join(MARK), crate::unix_now().to_string().as_bytes());
    Ok(true)
}

/// Copy `private`'s login back over the host file in `dir` when it is a
/// later one. Whether it did.
pub fn write_back(dir: &Path, private: &Path) -> std::io::Result<bool> {
    let _lock = lock(dir);
    write_back_locked(dir, private)
}

/// When the kernel last wrote a login back to the host file (unix seconds).
pub fn last_write_back(dir: &Path) -> Option<i64> {
    std::fs::read_to_string(dir.join(MARK))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The private logins of the tasks that share `worktree`'s parent: each
/// task's sandbox keeps its copy in a `<worktree>-provider` sibling.
pub fn private_copies(worktree: &Path) -> Vec<PathBuf> {
    let Some(parent) = worktree.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with("-provider"))
        .map(|e| e.path().join("claude").join(FILE))
        .filter(|p| is_regular_file(p))
        .collect()
}

/// Everything a launch does to the login before a sandbox starts, under one
/// lock: write back any later private pair (this task's own, from the attempt
/// before, or another task's still running), then seed `private` from the host
/// file. An unusable host file seeds nothing, and a copy left from an earlier
/// launch is removed with it, so an attempt never starts on a dead pair.
pub fn seed(dir: &Path, worktree: &Path, private: &Path) {
    let _lock = lock(dir);
    for copy in private_copies(worktree) {
        let _ = write_back_locked(dir, &copy);
    }
    let host = dir.join(FILE);
    if matches!(host_state(dir), Host::Usable(_)) {
        let _ = seed_copy(&host, private);
    } else {
        let _ = std::fs::remove_file(private);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn login(access: &str, refresh: &str, expires_at: i64) -> String {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{expires_at}}}}}"#
        )
    }

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn a_login_is_usable_only_with_both_tokens() {
        assert!(Creds::parse(&login("a", "r", NOW)).usable);
        assert!(!Creds::parse(&login("", "", 0)).usable);
        assert!(!Creds::parse(&login("a", "", NOW)).usable);
        assert!(!Creds::parse(&login("", "r", NOW)).usable);
        assert!(!Creds::parse("").usable);
        assert!(!Creds::parse("{}").usable);
        assert!(!Creds::parse("not json").usable);
    }

    #[test]
    fn expiry_reads_milliseconds_and_tolerates_seconds() {
        assert_eq!(Creds::parse(&login("a", "r", NOW)).expires_at_ms, NOW);
        assert_eq!(
            Creds::parse(&login("a", "r", NOW / 1000)).expires_at_ms,
            NOW
        );
        assert_eq!(Creds::parse(&login("", "", 0)).expires_at_ms, 0);
    }

    #[test]
    fn near_expiry_is_within_thirty_minutes() {
        let c = |ms| Creds::parse(&login("a", "r", ms));
        let w = REFRESH_WINDOW_MS;
        assert!(c(NOW + 29 * 60_000).near_expiry(NOW, w));
        assert!(!c(NOW + 31 * 60_000).near_expiry(NOW, w));
        assert!(c(NOW - 1).near_expiry(NOW, w));
        let short = refresh_window_ms(Duration::from_secs(60), Duration::ZERO);
        assert_eq!(short, REFRESH_WINDOW_MS);
    }

    #[test]
    fn a_long_timeout_widens_the_refresh_window() {
        // A two-hour attempt must not start on a token with 90 minutes left:
        // it would expire under the attempt and every concurrent one.
        let c = |ms| Creds::parse(&login("a", "r", ms));
        let w = refresh_window_ms(Duration::from_secs(2 * 3600), Duration::from_secs(600));
        assert_eq!(w, (120 + 10 + 5) * 60_000);
        assert!(c(NOW + 90 * 60_000).near_expiry(NOW, w));
        assert!(!c(NOW + 136 * 60_000).near_expiry(NOW, w));
        assert!(!c(NOW + 90 * 60_000).near_expiry(NOW, REFRESH_WINDOW_MS));
    }

    #[test]
    fn a_later_whole_private_login_is_written_back() {
        let c = |ms| Creds::parse(&login("a", "r", ms));
        assert!(should_write_back(c(NOW), c(NOW + 1), NOW));
        assert!(
            !should_write_back(c(NOW), c(NOW), NOW),
            "equal is not later"
        );
        assert!(!should_write_back(c(NOW), c(NOW - 1), NOW));
        let empty = Creds::parse(&login("", "", NOW + 9));
        assert!(
            !should_write_back(c(NOW), empty, NOW),
            "an empty copy never wins"
        );
    }

    #[test]
    fn an_unusable_host_file_takes_only_an_unexpired_private_login() {
        let c = |ms| Creds::parse(&login("a", "r", ms));
        let cleared = Creds::parse(&login("", "", 0));
        assert!(should_write_back(cleared, c(NOW + 60_000), NOW));
        assert!(!should_write_back(cleared, c(NOW - 60_000), NOW));
        assert!(!should_write_back(Creds::NONE, Creds::NONE, NOW));
    }

    #[test]
    fn replace_atomic_swaps_the_file_and_leaves_no_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(FILE);
        std::fs::write(&dest, "old").unwrap();
        replace_atomic(&dest, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [FILE], "the temporary sibling is renamed away");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the login is private to its owner");
    }

    #[test]
    fn replace_atomic_into_a_missing_directory_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        assert!(replace_atomic(&dir.path().join("gone/x"), b"new").is_err());
    }

    #[test]
    fn write_back_replaces_only_with_a_later_login() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join(FILE);
        let private = dir.path().join("private.json");
        let far = crate::unix_now() * 1000 + 8 * 3600 * 1000;
        std::fs::write(&host, login("old-a", "old-r", far)).unwrap();
        std::fs::write(&private, login("new-a", "new-r", far + 1000)).unwrap();
        assert!(write_back(dir.path(), &private).unwrap());
        assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
        assert!(last_write_back(dir.path()).is_some());
        // The same pair again is not later: nothing to do.
        assert!(!write_back(dir.path(), &private).unwrap());
        // An older one never goes back over a newer.
        std::fs::write(&private, login("older-a", "older-r", far - 1000)).unwrap();
        assert!(!write_back(dir.path(), &private).unwrap());
        assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
    }

    #[test]
    fn write_back_does_not_resurrect_a_logged_out_host() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private.json");
        std::fs::write(
            &private,
            login("a", "r", crate::unix_now() * 1000 + 3_600_000),
        )
        .unwrap();
        assert!(!write_back(dir.path(), &private).unwrap());
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn seed_copies_a_usable_host_login_and_removes_a_stale_copy_of_an_empty_one() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("claude");
        let worktree = root.path().join("work/task");
        let private = root.path().join("work/task-provider/claude").join(FILE);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        let far = crate::unix_now() * 1000 + 8 * 3600 * 1000;
        std::fs::write(dir.join(FILE), login("a", "r", far)).unwrap();
        seed(&dir, &worktree, &private);
        assert_eq!(
            std::fs::read_to_string(&private).unwrap(),
            login("a", "r", far)
        );
        // The host file is emptied and the private copy is expired: it must
        // neither be restored over the empty file nor left to seed anything.
        std::fs::write(dir.join(FILE), login("", "", 0)).unwrap();
        std::fs::write(&private, login("a", "r", 1_000)).unwrap();
        seed(&dir, &worktree, &private);
        assert!(!private.exists());
        assert_eq!(host_state(&dir), Host::Empty);
    }

    fn seed_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("claude");
        let worktree = root.path().join("work/task");
        let private = root.path().join("work/task-provider/claude").join(FILE);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        let far = crate::unix_now() * 1000 + 8 * 3600 * 1000;
        let text = login("a", "r", far);
        std::fs::write(dir.join(FILE), &text).unwrap();
        (root, dir, worktree, private, text)
    }

    #[test]
    fn seed_never_writes_through_a_private_symlink_to_the_host_login() {
        let (_root, dir, worktree, private, text) = seed_fixture();
        std::os::unix::fs::symlink(dir.join(FILE), &private).unwrap();
        seed(&dir, &worktree, &private);
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), text);
        assert!(is_regular_file(&private));
        assert_eq!(std::fs::read_to_string(&private).unwrap(), text);
    }

    #[test]
    fn seed_never_writes_through_a_private_symlink_to_an_unrelated_file() {
        let (root, dir, worktree, private, text) = seed_fixture();
        let victim = root.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        std::os::unix::fs::symlink(&victim, &private).unwrap();
        seed(&dir, &worktree, &private);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        assert!(is_regular_file(&private));
        assert_eq!(std::fs::read_to_string(&private).unwrap(), text);
    }

    #[test]
    fn a_symlinked_private_copy_is_not_read_for_write_back() {
        let (root, dir, _worktree, private, _text) = seed_fixture();
        let later = root.path().join("later");
        let far = crate::unix_now() * 1000 + 16 * 3600 * 1000;
        std::fs::write(&later, login("x", "y", far)).unwrap();
        std::os::unix::fs::symlink(&later, &private).unwrap();
        assert!(!write_back(&dir, &private).unwrap());
    }

    #[test]
    fn seed_first_writes_back_a_later_private_login_from_a_sibling_task() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("claude");
        let other = root.path().join("work/other-provider/claude").join(FILE);
        let worktree = root.path().join("work/task");
        let private = root.path().join("work/task-provider/claude").join(FILE);
        for d in [
            &dir,
            &worktree,
            other.parent().unwrap(),
            private.parent().unwrap(),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let far = crate::unix_now() * 1000 + 8 * 3600 * 1000;
        std::fs::write(dir.join(FILE), login("a", "dead", far)).unwrap();
        std::fs::write(&other, login("b", "live", far + 5000)).unwrap();
        seed(&dir, &worktree, &private);
        assert!(
            std::fs::read_to_string(dir.join(FILE))
                .unwrap()
                .contains("live")
        );
        assert!(std::fs::read_to_string(&private).unwrap().contains("live"));
    }
}
