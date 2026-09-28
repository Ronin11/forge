//! The `anthropic` row: the agent login the sandboxes are seeded from (see
//! `crate::login`).

use super::{Check, Status, check};
use crate::ctx::{Forge, Paths};
use crate::login::{self, Host};
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

/// The login file's row; `held`, the anthropic provider's hold row, leads
/// it: a FAIL for a hold keeps the file's detail beside its own words.
fn file_row(held: Option<Check>) -> Vec<Check> {
    let mut rows = login_file();
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

/// The login's expiry and whether the kernel has written a refreshed token
/// back to the host file lately. FAIL when the file holds an empty token,
/// which is what every attempt would die on.
fn login_file() -> Vec<Check> {
    let Some(dir) = login::config_dir() else {
        return Vec::new();
    };
    let file = dir.join(login::FILE);
    let now = unix_now();
    let wrote = match login::last_write_back(&dir) {
        Some(at) if now - at <= login::WRITE_BACK_WINDOW_SECS => {
            format!("write-back yes ({}m ago)", (now - at) / 60)
        }
        _ => "no write-back in the last 8h".to_string(),
    };
    let row = |status, detail: String, hint: &str| check("anthropic", status, detail, hint);
    vec![match login::host_state(&dir) {
        Host::Missing => row(
            Status::Warn,
            format!("no login at {}", file.display()),
            "run `claude login`, unless the provider uses an api_key_env",
        ),
        Host::Empty => row(
            Status::Fail,
            format!("{}: empty token; {wrote}", file.display()),
            "run `claude login`: attempts are refused until a login is there",
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
            if c.near_expiry(now * 1000) {
                row(
                    Status::Warn,
                    detail,
                    "the worker refreshes it on the host before the next launch",
                )
            } else {
                row(Status::Ok, detail, "")
            }
        }
    }]
}
