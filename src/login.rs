//! The agent logins belong to the kernel, not to any one sandbox.
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
//! replaces is kept once as `<file>.forge-prev`.
//!
//! codex rotates too. Its `auth.json` holds a ChatGPT login (`tokens`: an
//! access JWT that lives ten days, an id JWT, an `rt.1.` refresh token) and
//! its refresh tokens are single use: the CLI (0.154) says 'your refresh token
//! was already used' of a spent one. The refresh was not reproduced in a
//! sandbox (2026-09-28): with no write-back, a sandbox that refreshed would
//! have killed the operator's live codex login, the very outage. So codex's
//! login gets the same lock, seed record, guarded write-back and doctor row
//! as claude's, through its `Shape`; the host-side refresh before a launch
//! stays claude's alone (codex's token outlives any attempt by days).
//!
//! copilot does not rotate: its login is a GitHub token under `copilotTokens`
//! in `config.json` (or in the keychain, and then the file holds settings
//! only), with no expiry and no refresh token. It is seeded under the same
//! lock and never written back.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Held while the host file is read, replaced or seeded from.
const LOCK: &str = ".forge-credentials.lock";

/// Held while a probe refreshes the host file (see `refusal::refresh_on_host`),
/// so that a seed waits for the refreshed login rather than copying the one
/// about to rotate. Always taken before `LOCK`, never while holding it.
const PROBE_LOCK: &str = ".forge-refresh.lock";

/// The last refresh probe on the host: when it ran and the expiry it saw
/// before and after (see `ProbeRecord`). Beside `PROBE_LOCK`, and written
/// only under it.
const PROBE_MARK: &str = ".forge-refresh-probe";

/// A refresh probe runs at most this often, whatever it found.
pub const PROBE_MIN_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// After a probe the CLI did not refresh, the next waits until this long
/// before the expiry it saw: the CLI refreshes only near its own, shorter
/// margin, so a probe before then only runs it again for nothing.
pub const PROBE_BACKOFF_BEFORE_EXPIRY_MS: i64 = 5 * 60 * 1000;

/// Unix seconds of the last write-back, for `forge doctor`.
const MARK: &str = ".forge-writeback";

/// Where a seed's record is kept, under FORGE_HOME.
const SEEDS: &str = "login-seeds";

/// The most a private login may hold; the CLIs' files are well under 8 KiB.
const MAX_PRIVATE_BYTES: u64 = 64 * 1024;

/// The expiry of a login that has none (a copilot token, an API key).
pub const NEVER: i64 = i64::MAX;

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

/// One CLI's login, as far as the kernel handles it: where the file is, how
/// to read its expiry and whether its tokens are there, and what of it a
/// refresh rewrites (so what a seed's record keeps, and what a write-back may
/// change).
pub struct Shape {
    /// The CLI, and the directory of a task's private state its login is
    /// seeded into (`<worktree>-provider/<cli>`).
    pub cli: &'static str,
    /// Its config directory: this variable, else `home_dir` under `$HOME`.
    env: &'static str,
    home_dir: &'static str,
    /// The login file in that directory.
    pub file: &'static str,
    /// The row `forge doctor` gives it.
    pub row: &'static str,
    /// How the operator logs in again.
    pub login: &'static str,
    /// Token presence and expiry.
    creds: fn(&Value) -> Creds,
    /// The rotating refresh token, as a JSON pointer; `None` for a login
    /// that never rotates, which is never written back.
    refresh: Option<&'static str>,
    /// Every field a refresh rewrites, as JSON pointers.
    rotating: &'static [&'static str],
    /// Whether the rotating fields are shaped as the CLI writes them.
    shaped: fn(&Value) -> bool,
    /// The furthest ahead a freshly rotated login may expire.
    max_lifetime_ms: i64,
    /// The file holds the CLI's settings as well as its login: it is seeded
    /// even with no token in it.
    settings_too: bool,
}

pub const CLAUDE: Shape = Shape {
    cli: "claude",
    env: "CLAUDE_CONFIG_DIR",
    home_dir: ".claude",
    file: ".credentials.json",
    row: "anthropic",
    login: "claude login",
    creds: claude_creds,
    refresh: Some("/claudeAiOauth/refreshToken"),
    rotating: &[
        "/claudeAiOauth/accessToken",
        "/claudeAiOauth/refreshToken",
        "/claudeAiOauth/expiresAt",
    ],
    shaped: claude_shaped,
    // The CLI's tokens live hours; a day leaves room and no more.
    max_lifetime_ms: 24 * 3600 * 1000,
    settings_too: false,
};

pub const CODEX: Shape = Shape {
    cli: "codex",
    env: "CODEX_HOME",
    home_dir: ".codex",
    file: "auth.json",
    row: "codex-login",
    login: "codex login",
    creds: codex_creds,
    refresh: Some("/tokens/refresh_token"),
    rotating: &[
        "/tokens/access_token",
        "/tokens/id_token",
        "/tokens/refresh_token",
        "/last_refresh",
    ],
    shaped: codex_shaped,
    // The access token lives ten days; one more leaves room and no more.
    max_lifetime_ms: 11 * 24 * 3600 * 1000,
    settings_too: false,
};

pub const COPILOT: Shape = Shape {
    cli: "copilot",
    env: "COPILOT_HOME",
    home_dir: ".copilot",
    file: "config.json",
    row: "copilot-login",
    login: "copilot login",
    creds: copilot_creds,
    refresh: None,
    rotating: &[],
    shaped: |_| false,
    max_lifetime_ms: 0,
    settings_too: true,
};

/// Every login the sandboxes are seeded with.
pub const SHAPES: [&Shape; 3] = [&CLAUDE, &CODEX, &COPILOT];

/// What a credentials file says, as far as the kernel cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Creds {
    /// Every token the CLI needs is present and non-empty.
    pub usable: bool,
    /// When the login expires, in unix milliseconds; 0 when unknown, `NEVER`
    /// when it does not.
    pub expires_at_ms: i64,
}

impl Creds {
    /// Nothing there: no file, or one that says nothing.
    pub const NONE: Creds = Creds {
        usable: false,
        expires_at_ms: 0,
    };

    /// Whether a launch should refresh this login on the host first, given
    /// its refresh window (`refresh_window_ms`).
    pub fn near_expiry(&self, now_ms: i64, window_ms: i64) -> bool {
        self.expires_at_ms < now_ms.saturating_add(window_ms)
    }
}

/// `text` as JSON, past any whole-line `//` comments (copilot heads its
/// `config.json` with two).
fn json(text: &str) -> Option<Value> {
    let body: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&body).ok()
}

fn present(v: &Value) -> bool {
    v.as_str().is_some_and(|s| !s.trim().is_empty())
}

fn claude_creds(v: &Value) -> Creds {
    let o = &v["claudeAiOauth"];
    // Milliseconds, as the CLI writes them; a bare seconds stamp is read as
    // such rather than as 1970.
    let raw = o["expiresAt"].as_i64().unwrap_or(0).max(0);
    Creds {
        usable: present(&o["accessToken"]) && present(&o["refreshToken"]),
        expires_at_ms: if raw < 100_000_000_000 {
            raw * 1000
        } else {
            raw
        },
    }
}

/// A ChatGPT login expires with its access token (the JWT's `exp`); an API
/// key alone never does.
fn codex_creds(v: &Value) -> Creds {
    let t = &v["tokens"];
    if present(&t["access_token"]) && present(&t["refresh_token"]) {
        let exp = t["access_token"].as_str().and_then(jwt_exp).unwrap_or(0);
        return Creds {
            usable: true,
            expires_at_ms: exp.max(0).saturating_mul(1000),
        };
    }
    if present(&v["OPENAI_API_KEY"]) {
        return Creds {
            usable: true,
            expires_at_ms: NEVER,
        };
    }
    Creds::NONE
}

/// A GitHub token per logged-in user; none of them expires.
fn copilot_creds(v: &Value) -> Creds {
    let any = v["copilotTokens"]
        .as_object()
        .is_some_and(|m| m.values().any(present));
    if any {
        Creds {
            usable: true,
            expires_at_ms: NEVER,
        }
    } else {
        Creds::NONE
    }
}

/// `s` from unpadded base64url; `None` for anything else.
fn base64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A JWT's claims: three non-empty base64url parts, the middle one a JSON
/// object. The signature is not checked; nothing here trusts the claims but
/// for the expiry the CLI itself reads.
fn jwt_claims(s: &str) -> Option<Value> {
    let parts: Vec<&str> = s.split('.').collect();
    if s.len() > 16 * 1024 || parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    base64url(parts[0])?;
    base64url(parts[2])?;
    let claims: Value = serde_json::from_slice(&base64url(parts[1])?).ok()?;
    claims.is_object().then_some(claims)
}

/// A JWT's `exp`, unix seconds.
fn jwt_exp(s: &str) -> Option<i64> {
    jwt_claims(s)?["exp"].as_i64()
}

/// Which of the claude CLI's two OAuth tokens a string is checked against:
/// they have distinct prefixes, so one can never pass as the other.
#[derive(Clone, Copy)]
enum TokenKind {
    Access,
    Refresh,
}

/// Whether `s`, after `prefix`, is at least 32 `[A-Za-z0-9_-]` characters,
/// and `s` at most 4096 bytes in all. No `=`, `+`, `/`, `.`, `~`,
/// whitespace or escape is ever part of the body.
fn body_shaped(s: &str, prefix_len: usize) -> bool {
    s.len() <= 4096
        && s.len() >= prefix_len + 32
        && s.as_bytes()[prefix_len..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A token as the claude CLI writes one: `sk-ant-oat` (access) or
/// `sk-ant-ort` (refresh), two digits, a `-`, then the body.
fn token_shaped(kind: TokenKind, v: &Value) -> bool {
    let Some(s) = v.as_str() else {
        return false;
    };
    let prefix = match kind {
        TokenKind::Access => "sk-ant-oat",
        TokenKind::Refresh => "sk-ant-ort",
    };
    let Some(rest) = s.strip_prefix(prefix) else {
        return false;
    };
    let digits = rest.as_bytes();
    if digits.len() < 3
        || !digits[0].is_ascii_digit()
        || !digits[1].is_ascii_digit()
        || digits[2] != b'-'
    {
        return false;
    }
    body_shaped(s, prefix.len() + 3)
}

fn claude_shaped(v: &Value) -> bool {
    let o = &v["claudeAiOauth"];
    token_shaped(TokenKind::Access, &o["accessToken"])
        && token_shaped(TokenKind::Refresh, &o["refreshToken"])
}

/// A refresh token as codex writes one: `rt.`, a version in digits, `.`,
/// then the body.
fn codex_refresh_shaped(v: &Value) -> bool {
    let Some(s) = v.as_str() else {
        return false;
    };
    let Some(rest) = s.strip_prefix("rt.") else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    (1..=3).contains(&digits)
        && rest.as_bytes().get(digits) == Some(&b'.')
        && body_shaped(s, 3 + digits + 1)
}

/// An RFC 3339 stamp, as codex writes `last_refresh`.
fn stamp_shaped(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        (20..=40).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_digit() || b"-:T.Z+".contains(&b))
    })
}

fn codex_shaped(v: &Value) -> bool {
    let t = &v["tokens"];
    let jwt = |x: &Value| {
        x.as_str()
            .and_then(jwt_claims)
            .is_some_and(|c| c["exp"].is_i64())
    };
    jwt(&t["access_token"])
        && jwt(&t["id_token"])
        && codex_refresh_shaped(&t["refresh_token"])
        && stamp_shaped(&v["last_refresh"])
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

impl Shape {
    /// Read a login file's text. Never fails: text that is not the CLI's
    /// JSON is an unusable login that expires at 0.
    pub fn parse(&self, text: &str) -> Creds {
        json(text).map_or(Creds::NONE, |v| (self.creds)(&v))
    }

    pub fn host_state(&self, dir: &Path) -> Host {
        match std::fs::read_to_string(dir.join(self.file)) {
            Err(_) => Host::Missing,
            Ok(t) => match self.parse(&t) {
                c if c.usable => Host::Usable(c),
                _ => Host::Empty,
            },
        }
    }

    /// The CLI's config directory: its variable when set and not empty,
    /// else under `$HOME`.
    pub fn config_dir(&self) -> Option<PathBuf> {
        std::env::var_os(self.env)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(self.home_dir)))
    }

    /// The login a write-back replaced, kept for undoing an acceptance made
    /// in error by hand: one copy, the latest.
    pub fn prev(&self) -> String {
        format!("{}.forge-prev", self.file)
    }

    /// Whether a login this shape describes can rotate, and so be written
    /// back.
    pub fn rotates(&self) -> bool {
        self.refresh.is_some()
    }

    /// `text`'s login without the fields a token refresh rewrites.
    fn unrotating_fields(&self, text: &str) -> Option<Value> {
        let mut v = json(text)?;
        for p in self.rotating {
            let (parent, key) = p.rsplit_once('/')?;
            if let Some(o) = v.pointer_mut(parent).and_then(Value::as_object_mut) {
                o.remove(key);
            }
        }
        Some(v)
    }

    /// The private logins of the tasks that share `worktree`'s parent: each
    /// task's sandbox keeps its copy in a `<worktree>-provider` sibling.
    pub fn private_copies(&self, worktree: &Path) -> Vec<PathBuf> {
        let Some(parent) = worktree.parent() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(parent) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with("-provider"))
            .map(|e| e.path().join(self.cli).join(self.file))
            .filter(|p| is_regular_file(p))
            .collect()
    }

    /// `write_back_locked` every sibling private copy `private_copies` finds,
    /// then discard whichever sibling's own worktree no longer exists: past
    /// that write-back nothing will ever read the copy again (its task is
    /// gone), so leaving the directory for every later launch to scan and
    /// open under the login lock (docs/REVIEW-4.md #1.23) buys nothing.
    pub fn write_back_private_copies_locked(&self, dir: &Path, state: &Path, worktree: &Path) {
        let Some(parent) = worktree.parent() else {
            return;
        };
        for copy in self.private_copies(worktree) {
            let _ = self.write_back_locked(dir, state, &copy);
            let Some(provider_dir) = copy.parent().and_then(Path::parent) else {
                continue;
            };
            let stale = provider_dir
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| {
                    n.strip_suffix("-review-provider")
                        .or_else(|| n.strip_suffix("-provider"))
                })
                .is_some_and(|base| !parent.join(base).exists());
            if stale {
                let _ = std::fs::remove_dir_all(provider_dir);
            }
        }
    }

    /// `write_back` for a caller that holds the lock. `state` is FORGE_HOME,
    /// where the seed of `private` was recorded.
    pub fn write_back_locked(
        &self,
        dir: &Path,
        state: &Path,
        private: &Path,
    ) -> std::io::Result<bool> {
        if !self.rotates() {
            return Ok(false);
        }
        let host = dir.join(self.file);
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
        if !should_write_back(self.parse(&host_text), self.parse(&text), now)
            || !seed.could_have_produced(self, &text, now)
        {
            return Ok(false);
        }
        replace_atomic(&dir.join(self.prev()), host_text.as_bytes())?;
        replace_atomic(&host, text.as_bytes())?;
        let _ = replace_atomic(&dir.join(MARK), crate::unix_now().to_string().as_bytes());
        Ok(true)
    }

    /// Copy `private`'s login back over the host file in `dir` when it is a
    /// later one the CLI could have made from its seed. Whether it did.
    pub async fn write_back(
        &self,
        dir: &Path,
        state: &Path,
        private: &Path,
    ) -> std::io::Result<bool> {
        let _lock = lock(dir).await;
        self.write_back_locked(dir, state, private)
    }

    /// Everything a launch does to the login before a sandbox starts, under
    /// one lock: write back any later private login (this task's own, from
    /// the attempt before, or another task's still running), then seed
    /// `private` from the host file, recording in `state` (FORGE_HOME) what
    /// was seeded. An unusable host file seeds nothing (unless it holds the
    /// CLI's settings too), and a copy left from an earlier launch is removed
    /// with it, so an attempt never starts on a dead login. A refresh
    /// probe running on the host is waited out first (see `probe_lock`).
    pub async fn seed(&self, dir: &Path, state: &Path, worktree: &Path, private: &Path) {
        let _probe = probe_lock(dir).await;
        let _lock = lock(dir).await;
        self.write_back_private_copies_locked(dir, state, worktree);
        let seeded = std::fs::read_to_string(dir.join(self.file))
            .ok()
            .filter(|t| self.settings_too || self.parse(t).usable);
        // The record is made before the copy, so a copy that could rotate
        // never exists without the record that judges it. One that cannot
        // (no refresh token: an API key, copilot's) has no record, and is
        // never written back.
        let recorded = |text: &str| match Seed::of(self, text) {
            Some(_) => Seed::record(self, state, private, text).is_ok(),
            None => {
                Seed::forget(state, private);
                true
            }
        };
        match seeded {
            Some(text) if recorded(&text) && replace_atomic(private, text.as_bytes()).is_ok() => {}
            _ => {
                Seed::forget(state, private);
                let _ = std::fs::remove_file(private);
            }
        }
    }
}

/// Whether a private copy's login replaces the host file's: it is a whole
/// login that expires later. An unusable host file takes any private login
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

/// How long a waiter sleeps between tries of a held lock.
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(10);

/// Wait for the lock on `dir`'s login. A directory that cannot be locked (it
/// does not exist, so there is no login to race over) locks nothing. The
/// wait never blocks the thread: it is reached from every launch on a tokio
/// worker, and a worker parked in `flock` cannot drive the task that holds
/// the lock (a refresh probe) to release it.
pub async fn lock(dir: &Path) -> Lock {
    lock_named(dir, LOCK).await
}

/// Wait for the lock a refresh probe on `dir`'s login runs under. Taken
/// before `lock`, never while holding it.
pub async fn probe_lock(dir: &Path) -> Lock {
    lock_named(dir, PROBE_LOCK).await
}

async fn lock_named(dir: &Path, name: &str) -> Lock {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(name))
        .ok();
    if let Some(f) = &file {
        loop {
            // SAFETY: the descriptor is open for the life of `f`.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EWOULDBLOCK) => tokio::time::sleep(LOCK_RETRY).await,
                Some(libc::EINTR) => {}
                // Not lockable at all (as a blocking flock would have
                // failed): go on unlocked, as before.
                _ => break,
            }
        }
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
    /// Everything in the seeded file but the fields a refresh changes
    /// (claude's `subscriptionType` and `scopes`, codex's `account_id` and
    /// the rest).
    rest: Value,
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Seed {
    fn of(shape: &Shape, text: &str) -> Option<Seed> {
        let v = json(text)?;
        Some(Seed {
            refresh_sha256: sha256_hex(v.pointer(shape.refresh?)?.as_str()?),
            rest: shape.unrotating_fields(text)?,
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
    fn record(shape: &Shape, state: &Path, private: &Path, text: &str) -> std::io::Result<()> {
        let seed =
            Seed::of(shape, text).ok_or_else(|| std::io::Error::other("no login to record"))?;
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

    /// Whether `text` is a login the CLI could have written into a copy
    /// seeded with this: the refresh token rotated, the expiry within the
    /// CLI's token lifetime, the rotated fields shaped as the CLI writes
    /// them, every other field as seeded.
    fn could_have_produced(&self, shape: &Shape, text: &str, now_ms: i64) -> bool {
        let Some(v) = json(text) else {
            return false;
        };
        let Some(refresh) = shape.refresh.and_then(|p| v.pointer(p)?.as_str()) else {
            return false;
        };
        (shape.shaped)(&v)
            && sha256_hex(refresh) != self.refresh_sha256
            && shape.parse(text).expires_at_ms <= now_ms.saturating_add(shape.max_lifetime_ms)
            && shape
                .unrotating_fields(text)
                .is_some_and(|rest| rest == self.rest)
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

/// The last refresh probe run on a host login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeRecord {
    /// When it ran, unix milliseconds.
    pub at_ms: i64,
    /// The login's expiry before it ran.
    pub expires_before_ms: i64,
    /// The login's expiry after it ran; the same when it did not refresh.
    pub expires_after_ms: i64,
}

impl ProbeRecord {
    /// Whether the probe moved the login's expiry.
    pub fn refreshed(&self) -> bool {
        self.expires_after_ms != self.expires_before_ms
    }

    /// The earliest a next probe may run on a login expiring at
    /// `expires_at_ms`: five minutes after this one in any case, and after
    /// one that left that very expiry unchanged, not before five minutes
    /// short of it.
    pub fn next_probe_at(&self, expires_at_ms: i64) -> i64 {
        let floor = self.at_ms.saturating_add(PROBE_MIN_INTERVAL_MS);
        if !self.refreshed() && self.expires_after_ms == expires_at_ms {
            floor.max(expires_at_ms.saturating_sub(PROBE_BACKOFF_BEFORE_EXPIRY_MS))
        } else {
            floor
        }
    }
}

/// Whether a refresh probe may run now (`now_ms`) on a login expiring at
/// `expires_at_ms`, given the last probe run on it.
pub fn probe_due(last: Option<&ProbeRecord>, expires_at_ms: i64, now_ms: i64) -> bool {
    last.is_none_or(|r| now_ms >= r.next_probe_at(expires_at_ms))
}

/// The last refresh probe run on `dir`'s login, if any is recorded.
pub fn last_probe(dir: &Path) -> Option<ProbeRecord> {
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(PROBE_MARK)).ok()?).ok()?;
    Some(ProbeRecord {
        at_ms: v["at_ms"].as_i64()?,
        expires_before_ms: v["expires_before_ms"].as_i64()?,
        expires_after_ms: v["expires_after_ms"].as_i64()?,
    })
}

/// Record `r` as the last refresh probe on `dir`'s login. The caller holds
/// `probe_lock`.
pub fn record_probe(dir: &Path, r: &ProbeRecord) -> std::io::Result<()> {
    let body = serde_json::json!({
        "at_ms": r.at_ms,
        "expires_before_ms": r.expires_before_ms,
        "expires_after_ms": r.expires_after_ms,
    });
    replace_atomic(&dir.join(PROBE_MARK), body.to_string().as_bytes())
}

/// When the kernel last wrote a login back to the host file (unix seconds).
pub fn last_write_back(dir: &Path) -> Option<i64> {
    std::fs::read_to_string(dir.join(MARK))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests;
