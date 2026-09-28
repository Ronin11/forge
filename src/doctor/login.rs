//! The `anthropic`, `codex-login` and `copilot-login` rows: the agent logins
//! the sandboxes are seeded from (see `crate::login`).

use super::{Check, Status, check};
use crate::ctx::{Forge, Paths};
use crate::login::{self, Host, Shape};
use crate::store::Store;
use crate::unix_now;

/// The anthropic row, and a FAIL row for every provider still held for a
/// refused login (`crate::login_hold`). Each held provider is probed first:
/// logging in and running `forge doctor` is how the operator ends a hold,
/// and a hold the probe ends is said so on the provider's row.
pub(super) fn anthropic(paths: &Paths) -> Vec<Check> {
    let f = Store::open(&paths.home.join("forge.db"))
        .ok()
        .and_then(|s| Forge::open_with(paths.clone(), s).ok());
    let Some(f) = f else {
        return file_row(None);
    };
    let probed = crate::login_hold::probe_now(&f);
    let mut rows = Vec::new();
    let mut anthropic = None;
    for (provider, probe) in &probed {
        let row = if probe.ok {
            check(
                provider,
                Status::Ok,
                "the login answered a probe; hold released",
                "",
            )
        } else {
            let since = f.store.login_hold(provider).ok().flatten();
            let since = since.map_or(unix_now(), |h| h.since);
            let n = f.store.probes_since(provider, since).unwrap_or(0);
            let words = crate::login_hold::words(&f, provider, since);
            check(
                provider,
                Status::Fail,
                format!("{words}; {n} probe(s) refused, the last: {}", probe.detail),
                "attempts on it wait, uncounted, until its login answers",
            )
        };
        if provider == "anthropic" {
            anthropic = Some(row);
        } else {
            rows.push(row);
        }
    }
    let mut out = file_row(anthropic);
    out.extend(rows);
    out
}

/// The rows of the logins beside claude's: codex's and copilot's.
pub(super) fn others() -> Vec<Check> {
    [&login::CODEX, &login::COPILOT]
        .into_iter()
        .flat_map(login_file)
        .collect()
}

/// The login file's row; `held`, the anthropic provider's hold row, leads
/// it: a FAIL for a hold keeps the file's detail beside its own words.
fn file_row(held: Option<Check>) -> Vec<Check> {
    let mut rows = login_file(&login::CLAUDE);
    match (held, rows.first_mut()) {
        (Some(h), Some(row)) if h.status == Status::Fail => {
            row.detail = format!("{}; {}", h.detail, row.detail);
            row.status = Status::Fail;
            row.hint = h.hint;
        }
        (Some(h), Some(row)) => row.detail = format!("{}; {}", h.detail, row.detail),
        (Some(h), None) => rows.push(h),
        (None, _) => {}
    }
    rows
}

/// The last refresh probe on the host login, and whether it refreshed.
fn probe_note(p: &login::ProbeRecord, now: i64) -> String {
    let ago = (now - p.at_ms / 1000).max(0) / 60;
    if p.refreshed() {
        format!("last refresh probe {ago}m ago: refreshed")
    } else {
        format!(
            "last refresh probe {ago}m ago: not refreshed, the next not before {}",
            crate::render::utc(p.next_probe_at(p.expires_after_ms) / 1000)
        )
    }
}

/// The login's expiry and whether the kernel has written a refreshed token
/// back to the host file lately. FAIL when the file holds an empty token,
/// which is what every attempt would die on. Only claude's login is needed
/// by every host: no codex or copilot file is no more than a runner unused.
fn login_file(shape: &Shape) -> Vec<Check> {
    let Some(dir) = shape.config_dir() else {
        return Vec::new();
    };
    let file = dir.join(shape.file);
    let now = unix_now();
    let wrote = if !shape.rotates() {
        "it does not rotate: never written back".to_string()
    } else {
        match login::last_write_back(&dir) {
            Some(at) if now - at <= login::WRITE_BACK_WINDOW_SECS => {
                format!("write-back yes ({}m ago)", (now - at) / 60)
            }
            _ => "no write-back in the last 8h".to_string(),
        }
    };
    let claude = std::ptr::eq(shape, &login::CLAUDE);
    let wrote = match login::last_probe(&dir) {
        Some(p) if claude => format!("{wrote}; {}", probe_note(&p, now)),
        _ => wrote,
    };
    let row = |status, detail: String, hint: String| check(shape.row, status, detail, hint);
    vec![match shape.host_state(&dir) {
        Host::Missing if claude => row(
            Status::Warn,
            format!("no login at {}", file.display()),
            format!(
                "run `{}`, unless the provider uses an api_key_env",
                shape.login
            ),
        ),
        Host::Missing => row(
            Status::Ok,
            format!("no login at {}", file.display()),
            String::new(),
        ),
        // copilot keeps its token in the keychain when it can, and its file
        // then holds settings only.
        Host::Empty if !shape.rotates() => row(
            Status::Ok,
            format!(
                "no token in {} (the keychain or an api_key_env holds it)",
                file.display()
            ),
            String::new(),
        ),
        Host::Empty => row(
            Status::Fail,
            format!("{}: empty token; {wrote}", file.display()),
            format!(
                "run `{}`: attempts are refused until a login is there",
                shape.login
            ),
        ),
        Host::Usable(c) if c.expires_at_ms == login::NEVER => row(
            Status::Ok,
            format!("login does not expire; {wrote}"),
            String::new(),
        ),
        Host::Usable(c) => {
            let at = c.expires_at_ms / 1000;
            let detail = format!(
                "login expiresAt {} ({}); {wrote}",
                crate::render::utc(at),
                if at <= now {
                    "expired".to_string()
                } else {
                    format!("in {}m", (at - now) / 60)
                }
            );
            if c.near_expiry(now * 1000, crate::login::REFRESH_WINDOW_MS) {
                let hint = if claude {
                    "the worker refreshes it on the host before the next launch".to_string()
                } else {
                    format!(
                        "the next attempt's refresh is written back; if it expires first, run `{}`",
                        shape.login
                    )
                };
                row(Status::Warn, detail, hint)
            } else {
                row(Status::Ok, detail, String::new())
            }
        }
    }]
}
