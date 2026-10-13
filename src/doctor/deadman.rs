//! The outside dead-man (docs/OPS.md, "What doctor watches"). A box that
//! is down cannot say so: the worker pings `[alerts] deadman_url` once per
//! successful pass, and the worker unit's `ExecStopPost` pings it with
//! `/fail` appended, so a service that watches from elsewhere (healthchecks.io
//! or any URL that answers a GET) hears of silence and of every stop.
//! Unset, nothing is pinged and nothing runs.

use super::{Check, Status, check};
use crate::ctx::Forge;
use crate::store::{Mark, Store};
use anyhow::{Context, Result};
use std::time::Duration;

/// The mark the doctor row reads: `ok` or `failed`, and when.
pub const MARK: &str = "deadman.last_ping";

/// The longest a ping may take: a dead-man that hangs the pass would be
/// the failure it watches for.
const TIMEOUT: Duration = Duration::from_secs(8);

/// How often the mark is rewritten while pings keep succeeding.
const MARK_EVERY: i64 = 60;

/// Longer than this since the last ping and the row warns: the worker has
/// not completed a pass.
const STALE_SECS: i64 = 3600;

/// The `/fail` URL the unit's `ExecStopPost` pings.
pub fn fail_url(url: &str) -> String {
    format!("{}/fail", url.trim_end_matches('/'))
}

/// One GET of `url`; any non-2xx answer is an error.
pub async fn send(url: &str) -> Result<()> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("pinging {url}"))?
        .error_for_status()
        .with_context(|| format!("pinging {url}"))?;
    Ok(())
}

/// Ping the configured dead-man once and remember how it went. Never
/// fails the pass: a dead-man that cannot be reached is the doctor row's
/// to say, not a reason to stop working.
pub async fn ping(f: &Forge) {
    let Some(url) = f.alerts.deadman_url.as_deref() else {
        return;
    };
    let now = crate::unix_now();
    let (status, detail) = match send(url).await {
        Ok(()) => ("ok", String::new()),
        Err(e) => {
            eprintln!("dead-man ping failed: {e:#}");
            ("failed", format!("{e:#}"))
        }
    };
    let before = f.store.doctor_mark(MARK).ok().flatten();
    let unchanged = before
        .as_ref()
        .is_some_and(|m| m.status == status && m.detail == detail && now - m.at < MARK_EVERY);
    if !unchanged {
        let _ = f.store.set_doctor_mark(
            MARK,
            &Mark {
                status: status.into(),
                detail,
                at: now,
            },
        );
    }
}

/// The `deadman` row: when it was last pinged.
pub(super) fn row(home: &std::path::Path, store: &Store, now: i64) -> Check {
    let url = crate::config::load_home(home)
        .ok()
        .and_then(|c| c.alerts.deadman_url);
    if url.is_none() {
        return check(
            "deadman",
            Status::Ok,
            "not configured; nothing outside this machine hears of it going quiet",
            "set [alerts] deadman_url in config.toml, then re-run `forge init` so the worker unit carries the /fail ping",
        );
    }
    match store.doctor_mark(MARK).ok().flatten() {
        None => check(
            "deadman",
            Status::Warn,
            "configured, never pinged",
            "the worker pings once per successful pass; is it running?",
        ),
        Some(m) if m.status != "ok" => check(
            "deadman",
            Status::Warn,
            format!(
                "last ping failed ({}): {}",
                crate::render::utc(m.at),
                m.detail
            ),
            "check [alerts] deadman_url and that this machine can reach it",
        ),
        Some(m) if now - m.at > STALE_SECS => check(
            "deadman",
            Status::Warn,
            format!("last pinged {}", crate::render::utc(m.at)),
            "the worker has not completed a pass since; `systemctl --user status forge-worker`",
        ),
        Some(m) => check(
            "deadman",
            Status::Ok,
            format!("last pinged {}", crate::render::utc(m.at)),
            "",
        ),
    }
}

#[cfg(test)]
mod tests;
