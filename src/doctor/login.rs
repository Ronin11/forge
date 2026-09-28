//! The `anthropic` row: the agent login the sandboxes are seeded from (see
//! `crate::login`).

use super::{Check, Status, check};
use crate::login::{self, Host};
use crate::unix_now;

/// The login's expiry and whether the kernel has written a refreshed token
/// back to the host file lately. FAIL when the file holds an empty token,
/// which is what every attempt would die on.
pub(super) fn anthropic() -> Vec<Check> {
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
