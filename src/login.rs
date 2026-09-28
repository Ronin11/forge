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
    pub fn write_back(&self, dir: &Path, state: &Path, private: &Path) -> std::io::Result<bool> {
        let _lock = lock(dir);
        self.write_back_locked(dir, state, private)
    }

    /// Everything a launch does to the login before a sandbox starts, under
    /// one lock: write back any later private login (this task's own, from
    /// the attempt before, or another task's still running), then seed
    /// `private` from the host file, recording in `state` (FORGE_HOME) what
    /// was seeded. An unusable host file seeds nothing (unless it holds the
    /// CLI's settings too), and a copy left from an earlier launch is removed
    /// with it, so an attempt never starts on a dead login.
    pub fn seed(&self, dir: &Path, state: &Path, worktree: &Path, private: &Path) {
        let _lock = lock(dir);
        for copy in self.private_copies(worktree) {
            let _ = self.write_back_locked(dir, state, &copy);
        }
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

/// When the kernel last wrote a login back to the host file (unix seconds).
pub fn last_write_back(dir: &Path) -> Option<i64> {
    std::fs::read_to_string(dir.join(MARK))
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// One shape's fixtures: a login from two raw tokens and an expiry, and
    /// tokens shaped as its CLI writes them, tagged so an assertion can
    /// still find a distinguishing substring.
    struct Fix {
        shape: &'static Shape,
        /// A login holding `access` and `refresh` as they are, expiring at
        /// `at` (ms) where the shape records an expiry.
        raw: fn(access: &str, refresh: &str, at: i64) -> String,
        oat: fn(&str) -> String,
        ort: fn(&str) -> String,
    }

    impl Fix {
        fn login(&self, access: &str, refresh: &str, at: i64) -> String {
            (self.raw)(access, refresh, at)
        }
        /// A login the CLI could have written, tagged `a` and `r`.
        fn good(&self, a: &str, r: &str, at: i64) -> String {
            self.login(&(self.oat)(a), &(self.ort)(r), at)
        }
    }

    fn b64(bytes: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = c.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)) << (8 * (3 - c.len()));
            for i in 0..=c.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    /// A codex access token: a JWT expiring at `at` (ms), tagged.
    fn jwt(tag: &str, at: i64) -> String {
        let claims = serde_json::json!({"exp": at / 1000, "tag": tag}).to_string();
        format!(
            "{}.{}.{}",
            b64(br#"{"alg":"RS256"}"#),
            b64(claims.as_bytes()),
            b64(tag.as_bytes())
        )
    }

    fn claude_raw(access: &str, refresh: &str, at: i64) -> String {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{at}}}}}"#
        )
    }

    /// A codex login. An access token that is a plain tag (letters, digits,
    /// `-`, `_`) is made the tag of a JWT expiring at `at`, so the expiry is
    /// where the CLI keeps it; anything else goes in as it is.
    fn codex_raw(access: &str, refresh: &str, at: i64) -> String {
        let tag = !access.is_empty()
            && access
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        let access = if tag {
            jwt(access, at)
        } else {
            access.to_string()
        };
        serde_json::json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": jwt("id", at),
                "access_token": access,
                "refresh_token": refresh,
                "account_id": "acct-1",
            },
            "last_refresh": "2026-09-25T20:29:36.151046962Z",
        })
        .to_string()
    }

    fn copilot_raw(access: &str, _refresh: &str, _at: i64) -> String {
        format!(
            "// User settings belong in settings.json.\n// This file is managed automatically.\n{{\"firstLaunchAt\":\"2026-01-01\",\"copilotTokens\":{{\"https://github.com:octo\":\"{access}\"}}}}"
        )
    }

    static FIXES: [Fix; 3] = [
        Fix {
            shape: &CLAUDE,
            raw: claude_raw,
            oat: |t| format!("sk-ant-oat01-{t:x<32}"),
            ort: |t| format!("sk-ant-ort01-{t:x<32}"),
        },
        Fix {
            shape: &CODEX,
            raw: codex_raw,
            oat: |t| t.to_string(),
            ort: |t| format!("rt.1.{t:x<40}"),
        },
        Fix {
            shape: &COPILOT,
            raw: copilot_raw,
            oat: |t| format!("gho_{t:x<36}"),
            ort: |t| t.to_string(),
        },
    ];

    /// The shapes whose logins rotate, and so may be written back.
    fn rotating() -> impl Iterator<Item = &'static Fix> {
        FIXES.iter().filter(|f| f.shape.rotates())
    }

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn a_login_is_usable_only_with_every_token() {
        for f in &FIXES {
            let p = |t: &str| f.shape.parse(t).usable;
            let cli = f.shape.cli;
            assert!(p(&f.login("a", "r", NOW)), "{cli}");
            assert!(!p(&f.login("", "", 0)), "{cli}");
            assert!(!p(&f.login("", "r", NOW)), "{cli}");
            assert_eq!(p(&f.login("a", "", NOW)), !f.shape.rotates(), "{cli}");
            assert!(!p(""), "{cli}");
            assert!(!p("{}"), "{cli}");
            assert!(!p("not json"), "{cli}");
        }
    }

    #[test]
    fn expiry_is_read_where_each_cli_keeps_it() {
        let c = |f: &Fix, at| f.shape.parse(&f.login("a", "r", at)).expires_at_ms;
        let [claude, codex, copilot] = &FIXES;
        assert_eq!(c(claude, NOW), NOW);
        assert_eq!(c(claude, NOW / 1000), NOW, "seconds are read as such");
        assert_eq!(c(codex, NOW), NOW, "the access JWT's exp");
        assert_eq!(c(copilot, NOW), NEVER, "a GitHub token does not expire");
        for f in &FIXES {
            assert_eq!(f.shape.parse(&f.login("", "", 0)).expires_at_ms, 0);
        }
    }

    #[test]
    fn a_codex_api_key_is_a_login_that_never_expires() {
        let key = r#"{"OPENAI_API_KEY":"sk-proj-abc","tokens":null}"#;
        assert_eq!(
            CODEX.parse(key),
            Creds {
                usable: true,
                expires_at_ms: NEVER
            }
        );
        assert!(!CODEX.parse(key).near_expiry(NOW, REFRESH_WINDOW_MS));
    }

    #[test]
    fn near_expiry_is_within_thirty_minutes() {
        for f in rotating() {
            let c = |ms| f.shape.parse(&f.login("a", "r", ms));
            let w = REFRESH_WINDOW_MS;
            assert!(c(NOW + 29 * 60_000).near_expiry(NOW, w));
            assert!(!c(NOW + 31 * 60_000).near_expiry(NOW, w));
            assert!(c(NOW - 1000).near_expiry(NOW, w));
        }
        let short = refresh_window_ms(Duration::from_secs(60), Duration::ZERO);
        assert_eq!(short, REFRESH_WINDOW_MS);
    }

    #[test]
    fn a_long_timeout_widens_the_refresh_window() {
        // A two-hour attempt must not start on a token with 90 minutes left:
        // it would expire under the attempt and every concurrent one.
        let f = &FIXES[0];
        let c = |ms| f.shape.parse(&f.login("a", "r", ms));
        let w = refresh_window_ms(Duration::from_secs(2 * 3600), Duration::from_secs(600));
        assert_eq!(w, (120 + 10 + 5) * 60_000);
        assert!(c(NOW + 90 * 60_000).near_expiry(NOW, w));
        assert!(!c(NOW + 136 * 60_000).near_expiry(NOW, w));
        assert!(!c(NOW + 90 * 60_000).near_expiry(NOW, REFRESH_WINDOW_MS));
    }

    #[test]
    fn a_later_whole_private_login_is_written_back() {
        for f in rotating() {
            let c = |ms| f.shape.parse(&f.login("a", "r", ms));
            assert!(should_write_back(c(NOW), c(NOW + 1000), NOW));
            assert!(
                !should_write_back(c(NOW), c(NOW), NOW),
                "equal is not later"
            );
            assert!(!should_write_back(c(NOW), c(NOW - 1000), NOW));
            let empty = f.shape.parse(&f.login("", "", NOW + 9000));
            assert!(
                !should_write_back(c(NOW), empty, NOW),
                "an empty copy never wins"
            );
        }
    }

    #[test]
    fn an_unusable_host_file_takes_only_an_unexpired_private_login() {
        for f in rotating() {
            let c = |ms| f.shape.parse(&f.login("a", "r", ms));
            let cleared = f.shape.parse(&f.login("", "", 0));
            assert!(should_write_back(cleared, c(NOW + 60_000), NOW));
            assert!(!should_write_back(cleared, c(NOW - 60_000), NOW));
            assert!(!should_write_back(Creds::NONE, Creds::NONE, NOW));
        }
    }

    #[test]
    fn replace_atomic_swaps_the_file_and_leaves_no_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(CLAUDE.file);
        std::fs::write(&dest, "old").unwrap();
        replace_atomic(&dest, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [CLAUDE.file], "the temporary sibling is renamed away");
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
    struct Seeded {
        root: tempfile::TempDir,
        dir: PathBuf,
        state: PathBuf,
        worktree: PathBuf,
        private: PathBuf,
    }

    fn seeded(shape: &Shape, text: &str) -> Seeded {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join(shape.cli);
        let worktree = root.path().join("work/task");
        let private = root
            .path()
            .join("work/task-provider")
            .join(shape.cli)
            .join(shape.file);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        std::fs::write(dir.join(shape.file), text).unwrap();
        let state = state_of(&root);
        shape.seed(&dir, &state, &worktree, &private);
        Seeded {
            root,
            dir,
            state,
            worktree,
            private,
        }
    }

    fn far() -> i64 {
        crate::unix_now() * 1000 + 8 * 3600 * 1000
    }

    #[test]
    fn write_back_replaces_only_with_a_later_login() {
        for f in rotating() {
            let s = f.shape;
            let seed_text = f.login("old-a", "old-r", far());
            let t = seeded(s, &seed_text);
            assert_eq!(std::fs::read_to_string(&t.private).unwrap(), seed_text);
            let host = t.dir.join(s.file);
            std::fs::write(&t.private, f.good("new-a", "new-r", far() + 1000)).unwrap();
            assert!(s.write_back(&t.dir, &t.state, &t.private).unwrap(), "{}", s.cli);
            assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
            assert!(last_write_back(&t.dir).is_some());
            // The same login again is not later: nothing to do.
            assert!(!s.write_back(&t.dir, &t.state, &t.private).unwrap());
            // An older one never goes back over a newer.
            std::fs::write(&t.private, f.good("older-a", "older-r", far() - 1000)).unwrap();
            assert!(!s.write_back(&t.dir, &t.state, &t.private).unwrap());
            assert!(std::fs::read_to_string(&host).unwrap().contains("new-r"));
        }
    }

    #[test]
    fn an_accepted_rotation_keeps_the_login_it_replaced_once_and_privately() {
        use std::os::unix::fs::PermissionsExt;
        for f in rotating() {
            let s = f.shape;
            let seed_text = f.login("a0", "r0", far());
            let t = seeded(s, &seed_text);
            std::fs::write(&t.private, f.good("a1", "r1", far() + 1000)).unwrap();
            assert!(s.write_back(&t.dir, &t.state, &t.private).unwrap());
            let prev = t.dir.join(s.prev());
            assert_eq!(std::fs::read_to_string(&prev).unwrap(), seed_text);
            assert_eq!(
                std::fs::metadata(&prev).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::write(&t.private, f.good("a2", "r2", far() + 2000)).unwrap();
            assert!(s.write_back(&t.dir, &t.state, &t.private).unwrap());
            assert!(
                std::fs::read_to_string(&prev).unwrap().contains("r1"),
                "one copy: the latest replaced"
            );
        }
    }

    /// Whether the private copy `forged` writes is taken as a write-back
    /// over a host login seeded with `seed_text`, and the host file after;
    /// a rejection leaves it as seeded.
    fn offered(shape: &Shape, seed_text: &str, forged: impl FnOnce(&Path)) -> (bool, String) {
        let t = seeded(shape, seed_text);
        std::fs::remove_file(&t.private).unwrap();
        forged(&t.private);
        let took = shape.write_back(&t.dir, &t.state, &t.private).unwrap();
        assert_eq!(
            t.dir.join(shape.prev()).exists(),
            took,
            "a backup is made exactly when a login is accepted"
        );
        (took, std::fs::read_to_string(t.dir.join(shape.file)).unwrap())
    }

    #[test]
    fn a_private_login_that_did_not_rotate_the_refresh_token_is_rejected() {
        for f in rotating() {
            let seed_text = f.login("a0", &(f.ort)("r0"), far());
            let (took, host) = offered(f.shape, &seed_text, |p| {
                std::fs::write(p, f.login(&(f.oat)("other"), &(f.ort)("r0"), far() + 1000))
                    .unwrap();
            });
            assert!(!took, "{}", f.shape.cli);
            assert_eq!(host, seed_text);
        }
    }

    #[test]
    fn a_private_login_expiring_past_the_clis_token_lifetime_is_rejected() {
        for f in rotating() {
            let seed_text = f.login("a0", "r0", far());
            let life = f.shape.max_lifetime_ms;
            let (took, host) = offered(f.shape, &seed_text, |p| {
                let at = crate::unix_now() * 1000 + life + 60_000;
                std::fs::write(p, f.good("a1", "r1", at)).unwrap();
            });
            assert!(!took, "{}", f.shape.cli);
            assert_eq!(host, seed_text);
            let (took, _) = offered(f.shape, &seed_text, |p| {
                std::fs::write(p, f.good("a1", "r1", far() + 1000)).unwrap();
            });
            assert!(took, "{}: a login within the lifetime is taken", f.shape.cli);
        }
    }

    #[test]
    fn an_oversize_private_file_is_rejected() {
        for f in rotating() {
            let seed_text = f.login("a0", "r0", far());
            let (took, host) = offered(f.shape, &seed_text, |p| {
                let pad = " ".repeat(MAX_PRIVATE_BYTES as usize);
                std::fs::write(p, format!("{}{pad}", f.good("a1", "r1", far() + 1000))).unwrap();
            });
            assert!(!took);
            assert_eq!(host, seed_text);
        }
    }

    #[test]
    fn a_symlinked_private_file_is_rejected_even_when_it_holds_a_good_rotation() {
        for f in rotating() {
            let seed_text = f.login("a0", "r0", far());
            let (took, host) = offered(f.shape, &seed_text, |p| {
                let good = p.with_file_name("elsewhere.json");
                std::fs::write(&good, f.good("a1", "r1", far() + 1000)).unwrap();
                std::os::unix::fs::symlink(&good, p).unwrap();
            });
            assert!(!took);
            assert_eq!(host, seed_text);
        }
    }

    #[test]
    fn a_private_login_with_changed_unrotating_fields_is_rejected() {
        for f in rotating() {
            let seed_text = f.login("a0", "r0", far());
            // A field the seed had, changed; and one it did not, added.
            let changed = |text: String, key: &str| {
                let mut v = json(&text).unwrap();
                let o = v.as_object_mut().unwrap();
                let inner = o.values_mut().find(|x| x.is_object()).unwrap();
                inner
                    .as_object_mut()
                    .unwrap()
                    .insert(key.into(), Value::from("widened"));
                v.to_string()
            };
            for key in ["scopes", "account_id", "subscriptionType"] {
                let (took, host) = offered(f.shape, &seed_text, |p| {
                    let text = changed(f.good("a1", "r1", far() + 1000), key);
                    std::fs::write(p, text).unwrap();
                });
                assert!(!took, "{} {key}", f.shape.cli);
                assert_eq!(host, seed_text);
            }
            let (took, host) = offered(f.shape, &seed_text, |p| {
                std::fs::write(p, f.good("a1", "r1", far() + 1000)).unwrap();
            });
            assert!(took, "the same fields with a rotated token are taken");
            assert!(host.contains("r1"));
        }
    }

    #[test]
    fn a_claude_login_with_changed_scopes_or_subscription_is_rejected() {
        let access = format!("sk-ant-oat01-{:x<32}", "a1");
        let ort = |t: &str| format!("sk-ant-ort01-{t:x<32}");
        let with = |scopes: &str, sub: &str, refresh: &str, at: i64| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{at},"scopes":[{scopes}],"subscriptionType":"{sub}"}}}}"#
            )
        };
        let seed_text = with(r#""user:inference""#, "pro", "r0", far());
        let (took, _) = offered(&CLAUDE, &seed_text, |p| {
            let widened = with(
                r#""user:inference","user:admin""#,
                "pro",
                &ort("r1"),
                far() + 1000,
            );
            std::fs::write(p, widened).unwrap();
        });
        assert!(!took, "changed scopes");
        let (took, _) = offered(&CLAUDE, &seed_text, |p| {
            let text = with(r#""user:inference""#, "max", &ort("r1"), far() + 1000);
            std::fs::write(p, text).unwrap();
        });
        assert!(!took, "changed subscriptionType");
    }

    /// Token pairs each CLI would never write: (access, refresh), raw.
    fn misshapen(f: &Fix) -> Vec<(String, String)> {
        let x = |n| "x".repeat(n);
        let (oat, ort) = (f.oat, f.ort);
        let mut cases = vec![
            ("a 1".into(), ort("r1")),
            (oat("a1"), "r\\n1".into()),
            (oat("a1"), "".into()),
            ("".into(), ort("r1")),
            ("=".into(), "=".into()),
            (oat("a1"), x(40)),
            (oat("a1"), oat("swap")),
        ];
        match f.shape.cli {
            "claude" => cases.extend([
                (x(32), ort("r1")),
                (ort("swap"), ort("r1")),
                (format!("sk-ant-oat01-{}", x(31)), ort("r1")),
                (oat("a1"), format!("sk-ant-ort01-{}", x(31))),
                (format!("sk-ant-oat01-{}=", x(31)), ort("r1")),
                (format!("sk-ant-oat01-{}/{}", x(15), x(16)), ort("r1")),
            ]),
            "codex" => cases.extend([
                // Not a JWT: two parts, four, an empty one, a non-JSON body.
                ("aaaa.bbbb".into(), ort("r1")),
                ("aaaa.bbbb.cccc.dddd".into(), ort("r1")),
                (format!("{}..sig", b64(b"{}")), ort("r1")),
                (format!("{}.{}.sig", b64(b"{}"), b64(b"not json")), ort("r1")),
                // A JWT with no expiry, or with padding.
                (format!("{}.{}.sig", b64(b"{}"), b64(b"{}")), ort("r1")),
                (format!("{}.{}=.sig", b64(b"{}"), b64(br#"{"exp":1}"#)), ort("r1")),
                // A refresh token without its prefix, version or body.
                (oat("a1"), format!("rt.{}", x(40))),
                (oat("a1"), format!("rt..{}", x(40))),
                (oat("a1"), format!("rt.1.{}", x(31))),
                (oat("a1"), format!("rt.1.{}/{}", x(20), x(20))),
                (oat("a1"), format!("sk-ant-ort01-{}", x(32))),
            ]),
            _ => {}
        }
        cases
    }

    #[test]
    fn a_private_login_with_tokens_the_cli_would_not_write_is_rejected() {
        for f in rotating() {
            let seed_text = f.login("a0", "r0", far());
            for (access, refresh) in misshapen(f) {
                let (took, host) = offered(f.shape, &seed_text, |p| {
                    std::fs::write(p, f.login(&access, &refresh, far() + 1000)).unwrap();
                });
                assert!(!took, "{} {access:?} {refresh:?}", f.shape.cli);
                assert_eq!(host, seed_text);
            }
        }
    }

    #[test]
    fn a_codex_login_with_a_forged_refresh_stamp_is_rejected() {
        let f = &FIXES[1];
        let seed_text = f.login("a0", "r0", far());
        for stamp in [Value::from("x".repeat(60_000)), Value::from(1), Value::Null] {
            let (took, _) = offered(&CODEX, &seed_text, |p| {
                let mut v = json(&f.good("a1", "r1", far() + 1000)).unwrap();
                v["last_refresh"] = stamp.clone();
                std::fs::write(p, v.to_string()).unwrap();
            });
            assert!(!took, "{stamp:?}");
        }
    }

    #[test]
    fn a_private_login_with_no_recorded_seed_is_rejected() {
        for f in rotating() {
            let t = seeded(f.shape, &f.login("a0", "r0", far()));
            std::fs::remove_dir_all(&t.state).unwrap();
            std::fs::write(&t.private, f.good("a1", "r1", far() + 1000)).unwrap();
            assert!(!f.shape.write_back(&t.dir, &t.state, &t.private).unwrap());
        }
    }

    #[test]
    fn the_seed_record_holds_a_hash_of_the_refresh_token_never_the_token() {
        for f in rotating() {
            let refresh = (f.ort)("r0-secret");
            let t = seeded(f.shape, &f.login("a0-secret", &refresh, far()));
            let mut n = 0;
            for e in std::fs::read_dir(t.state.join(SEEDS)).unwrap().flatten() {
                let text = std::fs::read_to_string(e.path()).unwrap();
                assert!(!text.contains(&refresh) && !text.contains("a0-secret"), "{text}");
                n += 1;
            }
            assert_eq!(n, 1, "{}", f.shape.cli);
        }
    }

    #[test]
    fn write_back_does_not_resurrect_a_logged_out_host() {
        for f in rotating() {
            let t = seeded(f.shape, &f.login("a0", "r0", far()));
            std::fs::remove_file(t.dir.join(f.shape.file)).unwrap();
            std::fs::write(&t.private, f.good("a1", "r1", far())).unwrap();
            assert!(!f.shape.write_back(&t.dir, &t.state, &t.private).unwrap());
            assert!(!t.dir.join(f.shape.file).exists());
        }
    }

    #[test]
    fn seed_copies_a_usable_host_login_and_removes_a_stale_copy_of_an_empty_one() {
        for f in rotating() {
            let s = f.shape;
            let text = f.login("a", "r", far());
            let t = seeded(s, &text);
            assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
            assert!(t.state.join(SEEDS).is_dir());
            // The host file is emptied and the private copy is expired: it
            // must neither be restored over the empty file nor left to seed
            // anything.
            std::fs::write(t.dir.join(s.file), f.login("", "", 0)).unwrap();
            std::fs::write(&t.private, f.login("a", "r", 1_000)).unwrap();
            s.seed(&t.dir, &state_of(&t.root), &t.worktree, &t.private);
            assert!(!t.private.exists(), "{}", s.cli);
            assert_eq!(s.host_state(&t.dir), Host::Empty);
            assert!(!Seed::path(&t.state, &t.private).exists(), "no record either");
        }
    }

    #[test]
    fn a_login_that_cannot_rotate_is_seeded_without_a_record_and_never_written_back() {
        // copilot's file, with a token or without (the keychain holds it),
        // and codex's API key.
        let with_token = copilot_raw(&format!("gho_{:x<36}", "t"), "", 0);
        let settings_only = "// managed\n{\"firstLaunchAt\":\"2026-01-01\"}";
        let key = r#"{"OPENAI_API_KEY":"sk-proj-abc","tokens":null}"#;
        for (s, text) in [
            (&COPILOT, with_token.as_str()),
            (&COPILOT, settings_only),
            (&CODEX, key),
        ] {
            let t = seeded(s, text);
            assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
            assert!(!Seed::path(&t.state, &t.private).exists());
            let later = copilot_raw(&format!("gho_{:x<36}", "planted"), "", 0);
            std::fs::write(&t.private, &later).unwrap();
            assert!(!s.write_back(&t.dir, &t.state, &t.private).unwrap());
            s.seed(&t.dir, &t.state, &t.worktree, &t.private);
            assert_eq!(std::fs::read_to_string(t.dir.join(s.file)).unwrap(), text);
            assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
            assert!(!t.dir.join(s.prev()).exists());
        }
    }

    #[test]
    fn seed_never_writes_through_a_private_symlink_to_the_host_login() {
        for f in &FIXES {
            let text = f.login("a", "r", far());
            let t = seeded(f.shape, &text);
            let host = t.dir.join(f.shape.file);
            std::fs::remove_file(&t.private).unwrap();
            std::os::unix::fs::symlink(&host, &t.private).unwrap();
            f.shape.seed(&t.dir, &t.state, &t.worktree, &t.private);
            assert_eq!(std::fs::read_to_string(&host).unwrap(), text);
            assert!(is_regular_file(&t.private));
            assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
        }
    }

    #[test]
    fn seed_never_writes_through_a_private_symlink_to_an_unrelated_file() {
        for f in &FIXES {
            let text = f.login("a", "r", far());
            let t = seeded(f.shape, &text);
            let victim = t.root.path().join("victim");
            std::fs::write(&victim, "precious").unwrap();
            std::fs::remove_file(&t.private).unwrap();
            std::os::unix::fs::symlink(&victim, &t.private).unwrap();
            f.shape.seed(&t.dir, &t.state, &t.worktree, &t.private);
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
            assert!(is_regular_file(&t.private));
            assert_eq!(std::fs::read_to_string(&t.private).unwrap(), text);
        }
    }

    #[test]
    fn seed_forgets_the_records_of_copies_that_are_gone() {
        for f in rotating() {
            let s = f.shape;
            let t = seeded(s, &f.login("a", "r", far()));
            let old = Seed::path(&t.state, &t.private);
            assert!(old.exists());
            std::fs::remove_dir_all(t.private.parent().unwrap().parent().unwrap()).unwrap();
            let next = t
                .root
                .path()
                .join("work/next-provider")
                .join(s.cli)
                .join(s.file);
            std::fs::create_dir_all(next.parent().unwrap()).unwrap();
            s.seed(&t.dir, &t.state, &t.worktree, &next);
            assert!(!old.exists());
            assert!(Seed::path(&t.state, &next).exists());
        }
    }

    #[test]
    fn seed_first_writes_back_a_later_private_login_from_a_sibling_task() {
        for f in rotating() {
            let s = f.shape;
            let t = seeded(s, &f.login("a", "dead", far()));
            // A sibling task seeded from the same host file, then rotated.
            let other = t
                .root
                .path()
                .join("work/other-provider")
                .join(s.cli)
                .join(s.file);
            std::fs::create_dir_all(other.parent().unwrap()).unwrap();
            s.seed(&t.dir, &t.state, &t.worktree, &other);
            std::fs::write(&other, f.good("b", "live", far() + 5000)).unwrap();
            std::fs::remove_file(&t.private).unwrap();
            s.seed(&t.dir, &t.state, &t.worktree, &t.private);
            let host = std::fs::read_to_string(t.dir.join(s.file)).unwrap();
            assert!(host.contains("live"), "{}", s.cli);
            assert!(std::fs::read_to_string(&t.private).unwrap().contains("live"));
        }
    }

    #[test]
    fn each_login_keeps_its_own_lock_mark_and_backup_beside_its_file() {
        let t = seeded(&CODEX, &codex_raw("a0", "r0", far()));
        let good = codex_raw("a1", &format!("rt.1.{:x<40}", "r1"), far() + 1000);
        std::fs::write(&t.private, good).unwrap();
        assert!(CODEX.write_back(&t.dir, &t.state, &t.private).unwrap());
        for name in [LOCK, MARK, "auth.json.forge-prev"] {
            assert!(t.dir.join(name).exists(), "{name}");
        }
        // A claude copy beside it is not codex's to judge.
        let claude = t.root.path().join("work/task-provider/claude").join(CLAUDE.file);
        std::fs::create_dir_all(claude.parent().unwrap()).unwrap();
        std::fs::write(&claude, "{}").unwrap();
        assert_eq!(CODEX.private_copies(&t.worktree), [t.private.clone()]);
    }
}
