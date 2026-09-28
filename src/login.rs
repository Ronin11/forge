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
//!
//! The private copy is a file the sandbox can write, so a later pair in it is
//! not taken on its say-so: only one the CLI could have produced from the
//! seed is (see `Seed::could_have_produced`), judged against a record of the
//! seed kept in FORGE_HOME, where no sandbox can write. The file a write-back
//! replaces is kept once as `.credentials.json.forge-prev`.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
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

/// The login a write-back replaced, kept for undoing an acceptance made in
/// error by hand: one copy, the latest.
pub const PREV: &str = ".credentials.json.forge-prev";

/// Where a seed's record is kept, under FORGE_HOME.
const SEEDS: &str = "login-seeds";

/// The most a private pair may hold; the CLI's file is well under 4 KiB.
const MAX_PRIVATE_BYTES: u64 = 64 * 1024;

/// The furthest ahead a freshly rotated pair may expire. The CLI's tokens
/// live hours; a day leaves room and no more.
const MAX_LIFETIME_MS: i64 = 24 * 3600 * 1000;

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

/// What was seeded into one private copy, as the kernel recorded it at seed
/// time: the sandbox never sees this file.
struct Seed {
    /// SHA-256 of the seeded refresh token, hex.
    refresh_sha256: String,
    /// Everything in the seeded file but the three fields a refresh changes
    /// (`subscriptionType`, `scopes` and the rest).
    rest: Value,
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `text`'s login without the fields a token refresh rewrites.
fn unrotating_fields(text: &str) -> Option<Value> {
    let mut v = serde_json::from_str::<Value>(text).ok()?;
    let o = v.get_mut("claudeAiOauth")?.as_object_mut()?;
    for k in ["accessToken", "refreshToken", "expiresAt"] {
        o.remove(k);
    }
    Some(v)
}

/// A token as the CLI writes one: a single run of URL-safe characters.
fn token_shaped(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        (1..=4096).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
    })
}

impl Seed {
    fn of(text: &str) -> Option<Seed> {
        let v = serde_json::from_str::<Value>(text).ok()?;
        Some(Seed {
            refresh_sha256: sha256_hex(v["claudeAiOauth"]["refreshToken"].as_str()?),
            rest: unrotating_fields(text)?,
        })
    }

    /// Where `private`'s record lives in `state` (FORGE_HOME).
    fn path(state: &Path, private: &Path) -> PathBuf {
        let key = sha256_hex(&private.to_string_lossy());
        state.join(SEEDS).join(format!("{}.json", &key[..32]))
    }

    fn load(state: &Path, private: &Path) -> Option<Seed> {
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(Seed::path(state, private)).ok()?)
                .ok()?;
        Some(Seed {
            refresh_sha256: v["refresh_sha256"].as_str()?.to_string(),
            rest: v["rest"].clone(),
        })
    }

    /// Record the seed of `private` (the text of the host file it is a copy
    /// of), and forget the records of copies that are gone.
    fn record(state: &Path, private: &Path, text: &str) -> std::io::Result<()> {
        let seed = Seed::of(text).ok_or_else(|| std::io::Error::other("no login to record"))?;
        let dir = state.join(SEEDS);
        std::fs::create_dir_all(&dir)?;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let gone = std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v["private"].as_str().map(|p| !Path::new(p).exists()))
                    .unwrap_or(false);
                if gone {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let body = serde_json::json!({
            "private": private.to_string_lossy(),
            "refresh_sha256": seed.refresh_sha256,
            "rest": seed.rest,
        });
        replace_atomic(&Seed::path(state, private), body.to_string().as_bytes())
    }

    fn forget(state: &Path, private: &Path) {
        let _ = std::fs::remove_file(Seed::path(state, private));
    }

    /// Whether `text` is a pair the CLI could have written into a copy seeded
    /// with this: the refresh token rotated, the expiry at most a day ahead,
    /// both tokens shaped as the CLI writes them, every other field as seeded.
    fn could_have_produced(&self, text: &str, now_ms: i64) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return false;
        };
        let o = &v["claudeAiOauth"];
        token_shaped(&o["accessToken"])
            && token_shaped(&o["refreshToken"])
            && sha256_hex(o["refreshToken"].as_str().unwrap_or_default()) != self.refresh_sha256
            && Creds::parse(text).expires_at_ms <= now_ms + MAX_LIFETIME_MS
            && unrotating_fields(text).is_some_and(|rest| rest == self.rest)
    }
}

/// The text of the private copy at `path`, if it is a regular file (never a
/// symlink, which a sandbox can plant, and never a pipe) of at most 64 KiB.
/// The bound holds on the bytes read, not on a size looked at earlier.
fn read_private(path: &Path) -> Option<String> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !f.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut f)
        .take(MAX_PRIVATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_PRIVATE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// `write_back` for a caller that holds the lock. `state` is FORGE_HOME,
/// where the seed of `private` was recorded.
pub fn write_back_locked(dir: &Path, state: &Path, private: &Path) -> std::io::Result<bool> {
    let host = dir.join(FILE);
    let Ok(host_text) = std::fs::read_to_string(&host) else {
        // Logged out (or never in): a stale copy does not undo that.
        return Ok(false);
    };
    let Some(text) = read_private(private) else {
        return Ok(false);
    };
    let now = unix_ms();
    let Some(seed) = Seed::load(state, private) else {
        return Ok(false);
    };
    if !should_write_back(Creds::parse(&host_text), Creds::parse(&text), now)
        || !seed.could_have_produced(&text, now)
    {
        return Ok(false);
    }
    replace_atomic(&dir.join(PREV), host_text.as_bytes())?;
    replace_atomic(&host, text.as_bytes())?;
    let _ = replace_atomic(&dir.join(MARK), crate::unix_now().to_string().as_bytes());
    Ok(true)
}

/// Copy `private`'s login back over the host file in `dir` when it is a
/// later one the CLI could have made from its seed. Whether it did.
pub fn write_back(dir: &Path, state: &Path, private: &Path) -> std::io::Result<bool> {
    let _lock = lock(dir);
    write_back_locked(dir, state, private)
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
/// file, recording in `state` (FORGE_HOME) what was seeded. An unusable host
/// file seeds nothing, and a copy left from an earlier
/// launch is removed with it, so an attempt never starts on a dead pair.
pub fn seed(dir: &Path, state: &Path, worktree: &Path, private: &Path) {
    let _lock = lock(dir);
    for copy in private_copies(worktree) {
        let _ = write_back_locked(dir, state, &copy);
    }
    let seeded = match host_state(dir) {
        Host::Usable(_) => std::fs::read_to_string(dir.join(FILE)).ok(),
        _ => None,
    };
    // The record is made before the copy, so a copy never exists without the
    // record that judges it.
    match seeded {
        Some(text)
            if Seed::record(state, private, &text).is_ok()
                && replace_atomic(private, text.as_bytes()).is_ok() => {}
        _ => {
            Seed::forget(state, private);
            let _ = std::fs::remove_file(private);
        }
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

    fn state_of(root: &tempfile::TempDir) -> PathBuf {
        root.path().join("forge-home")
    }

    /// A host login `text`, seeded into a private copy the way a launch does.
    fn seeded(text: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("claude");
        let worktree = root.path().join("work/task");
        let private = root.path().join("work/task-provider/claude").join(FILE);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        std::fs::write(dir.join(FILE), text).unwrap();
        let state = state_of(&root);
        seed(&dir, &state, &worktree, &private);
        (root, dir, state, worktree, private)
    }

    fn far() -> i64 {
        crate::unix_now() * 1000 + 8 * 3600 * 1000
    }

    #[test]
    fn write_back_replaces_only_with_a_later_login() {
        let (_root, dir, state, _wt, private) = seeded(&login("old-a", "old-r", far()));
        assert_eq!(
            std::fs::read_to_string(&private).unwrap(),
            login("old-a", "old-r", far())
        );
        let host = dir.join(FILE);
        std::fs::write(&private, login("new-a", "new-r", far() + 1000)).unwrap();
        assert!(write_back(&dir, &state, &private).unwrap());
        assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
        assert!(last_write_back(&dir).is_some());
        // The same pair again is not later: nothing to do.
        assert!(!write_back(&dir, &state, &private).unwrap());
        // An older one never goes back over a newer.
        std::fs::write(&private, login("older-a", "older-r", far() - 1000)).unwrap();
        assert!(!write_back(&dir, &state, &private).unwrap());
        assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
    }

    #[test]
    fn an_accepted_rotation_keeps_the_login_it_replaced_once_and_privately() {
        use std::os::unix::fs::PermissionsExt;
        let seed_text = login("a0", "r0", far());
        let (_root, dir, state, _wt, private) = seeded(&seed_text);
        std::fs::write(&private, login("a1", "r1", far() + 1000)).unwrap();
        assert!(write_back(&dir, &state, &private).unwrap());
        let prev = dir.join(PREV);
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), seed_text);
        assert_eq!(
            std::fs::metadata(&prev).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&private, login("a2", "r2", far() + 2000)).unwrap();
        assert!(write_back(&dir, &state, &private).unwrap());
        assert!(
            std::fs::read_to_string(&prev).unwrap().contains("r1"),
            "one copy: the latest replaced"
        );
    }

    /// The host file and its bytes, after `private` is offered as a
    /// write-back; a rejection leaves both as seeded.
    fn offered(seed_text: &str, forged: impl FnOnce(&Path)) -> (bool, String) {
        let (_root, dir, state, _wt, private) = seeded(seed_text);
        std::fs::remove_file(&private).unwrap();
        forged(&private);
        let took = write_back(&dir, &state, &private).unwrap();
        assert_eq!(
            dir.join(PREV).exists(),
            took,
            "a backup is made exactly when a pair is accepted"
        );
        (took, std::fs::read_to_string(dir.join(FILE)).unwrap())
    }

    #[test]
    fn a_private_pair_that_did_not_rotate_the_refresh_token_is_rejected() {
        let seed_text = login("a0", "r0", far());
        let (took, host) = offered(&seed_text, |p| {
            std::fs::write(p, login("other-access", "r0", far() + 1000)).unwrap();
        });
        assert!(!took);
        assert_eq!(host, seed_text);
    }

    #[test]
    fn a_private_pair_expiring_more_than_a_day_out_is_rejected() {
        let seed_text = login("a0", "r0", far());
        let day = 24 * 3600 * 1000;
        let (took, host) = offered(&seed_text, |p| {
            let at = crate::unix_now() * 1000 + day + 60_000;
            std::fs::write(p, login("a1", "r1", at)).unwrap();
        });
        assert!(!took);
        assert_eq!(host, seed_text);
        let (took, _) = offered(&seed_text, |p| {
            std::fs::write(p, login("a1", "r1", far() + 1000)).unwrap();
        });
        assert!(took, "a pair within the day is taken");
    }

    #[test]
    fn an_oversize_private_file_is_rejected() {
        let seed_text = login("a0", "r0", far());
        let (took, host) = offered(&seed_text, |p| {
            let pad = " ".repeat(MAX_PRIVATE_BYTES as usize);
            std::fs::write(p, format!("{}{pad}", login("a1", "r1", far() + 1000))).unwrap();
        });
        assert!(!took);
        assert_eq!(host, seed_text);
    }

    #[test]
    fn a_symlinked_private_file_is_rejected_even_when_it_holds_a_good_rotation() {
        let seed_text = login("a0", "r0", far());
        let (took, host) = offered(&seed_text, |p| {
            let good = p.with_file_name("elsewhere.json");
            std::fs::write(&good, login("a1", "r1", far() + 1000)).unwrap();
            std::os::unix::fs::symlink(&good, p).unwrap();
        });
        assert!(!took);
        assert_eq!(host, seed_text);
    }

    #[test]
    fn a_private_pair_with_changed_scopes_or_subscription_is_rejected() {
        let with = |scopes: &str, sub: &str, refresh: &str, at: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"{refresh}","expiresAt":{at},"scopes":[{scopes}],"subscriptionType":"{sub}"}}}}"#
            )
        };
        let seed_text = with(r#""user:inference""#, "pro", "r0", far());
        let (took, host) = offered(&seed_text, |p| {
            let widened = with(r#""user:inference","user:admin""#, "pro", "r1", far() + 1000);
            std::fs::write(p, widened).unwrap();
        });
        assert!(!took, "changed scopes");
        assert_eq!(host, seed_text);
        let (took, _) = offered(&seed_text, |p| {
            std::fs::write(p, with(r#""user:inference""#, "max", "r1", far() + 1000)).unwrap();
        });
        assert!(!took, "changed subscriptionType");
        let (took, host) = offered(&seed_text, |p| {
            std::fs::write(p, with(r#""user:inference""#, "pro", "r1", far() + 1000)).unwrap();
        });
        assert!(took, "the same fields with a rotated token are taken");
        assert!(host.contains("r1"));
    }

    #[test]
    fn a_private_pair_with_tokens_the_cli_would_not_write_is_rejected() {
        let seed_text = login("a0", "r0", far());
        for (access, refresh) in [("a 1", "r1"), ("a1", "r\\n1"), ("a1", ""), ("", "r1")] {
            let (took, host) = offered(&seed_text, |p| {
                std::fs::write(p, login(access, refresh, far() + 1000)).unwrap();
            });
            assert!(!took, "{access:?} {refresh:?}");
            assert_eq!(host, seed_text);
        }
    }

    #[test]
    fn a_private_pair_with_no_recorded_seed_is_rejected() {
        let (_root, dir, state, _wt, private) = seeded(&login("a0", "r0", far()));
        std::fs::remove_dir_all(&state).unwrap();
        std::fs::write(&private, login("a1", "r1", far() + 1000)).unwrap();
        assert!(!write_back(&dir, &state, &private).unwrap());
    }

    #[test]
    fn the_seed_record_holds_a_hash_of_the_refresh_token_never_the_token() {
        let (_root, _dir, state, _wt, _private) = seeded(&login("a0", "r0-secret", far()));
        for e in std::fs::read_dir(state.join(SEEDS)).unwrap().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap();
            assert!(!text.contains("r0-secret") && !text.contains("a0\""), "{text}");
        }
    }

    #[test]
    fn write_back_does_not_resurrect_a_logged_out_host() {
        let (_root, dir, state, _wt, private) = seeded(&login("a0", "r0", far()));
        std::fs::remove_file(dir.join(FILE)).unwrap();
        std::fs::write(&private, login("a1", "r1", far())).unwrap();
        assert!(!write_back(&dir, &state, &private).unwrap());
        assert!(!dir.join(FILE).exists());
    }

    #[test]
    fn seed_copies_a_usable_host_login_and_removes_a_stale_copy_of_an_empty_one() {
        let text = login("a", "r", far());
        let (root, dir, state, worktree, private) = seeded(&text);
        assert_eq!(std::fs::read_to_string(&private).unwrap(), text);
        assert!(state.join(SEEDS).is_dir());
        // The host file is emptied and the private copy is expired: it must
        // neither be restored over the empty file nor left to seed anything.
        std::fs::write(dir.join(FILE), login("", "", 0)).unwrap();
        std::fs::write(&private, login("a", "r", 1_000)).unwrap();
        seed(&dir, &state_of(&root), &worktree, &private);
        assert!(!private.exists());
        assert_eq!(host_state(&dir), Host::Empty);
        assert!(!Seed::path(&state, &private).exists(), "no record either");
    }

    #[test]
    fn seed_never_writes_through_a_private_symlink_to_the_host_login() {
        let text = login("a", "r", far());
        let (_root, dir, state, worktree, private) = seeded(&text);
        std::fs::remove_file(&private).unwrap();
        std::os::unix::fs::symlink(dir.join(FILE), &private).unwrap();
        seed(&dir, &state, &worktree, &private);
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), text);
        assert!(is_regular_file(&private));
        assert_eq!(std::fs::read_to_string(&private).unwrap(), text);
    }

    #[test]
    fn seed_never_writes_through_a_private_symlink_to_an_unrelated_file() {
        let text = login("a", "r", far());
        let (root, dir, state, worktree, private) = seeded(&text);
        let victim = root.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        std::fs::remove_file(&private).unwrap();
        std::os::unix::fs::symlink(&victim, &private).unwrap();
        seed(&dir, &state, &worktree, &private);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        assert!(is_regular_file(&private));
        assert_eq!(std::fs::read_to_string(&private).unwrap(), text);
    }

    #[test]
    fn seed_forgets_the_records_of_copies_that_are_gone() {
        let (_root, dir, state, worktree, private) = seeded(&login("a", "r", far()));
        let old = Seed::path(&state, &private);
        assert!(old.exists());
        std::fs::remove_dir_all(private.parent().unwrap().parent().unwrap()).unwrap();
        let next = _root.path().join("work/next-provider/claude").join(FILE);
        std::fs::create_dir_all(next.parent().unwrap()).unwrap();
        seed(&dir, &state, &worktree, &next);
        assert!(!old.exists());
        assert!(Seed::path(&state, &next).exists());
    }

    #[test]
    fn seed_first_writes_back_a_later_private_login_from_a_sibling_task() {
        let (root, dir, state, worktree, private) = seeded(&login("a", "dead", far()));
        // A sibling task seeded from the same host file, then rotated.
        let other = root.path().join("work/other-provider/claude").join(FILE);
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        seed(&dir, &state, &worktree, &other);
        std::fs::write(&other, login("b", "live", far() + 5000)).unwrap();
        std::fs::remove_file(&private).unwrap();
        seed(&dir, &state, &worktree, &private);
        assert!(
            std::fs::read_to_string(dir.join(FILE))
                .unwrap()
                .contains("live")
        );
        assert!(std::fs::read_to_string(&private).unwrap().contains("live"));
    }
}
