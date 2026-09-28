//! A provider whose agent login was refused is held until a person logs in.
//!
//! From 2026-09-26 23:48 to 02:24 the claude CLI's OAuth session had expired
//! and could not refresh: every attempt exited in half a second with
//! 'Failed to authenticate', the kernel read 'agent exit 1', and 40 attempts
//! on 15 tasks were spent while the operator's morning `claude login` would
//! have fixed every one. A refused login (`agent::refusal::login_failure`)
//! is now a refusal like a spent window: the attempt is refunded and the
//! provider held. Unlike a window the hold names no time. It ends when a
//! one-token probe answers: the worker probes a held provider when it
//! starts and every ten minutes after, recording each probe and its cost,
//! and `forge doctor` probes too, so logging in and running it is the
//! operator's whole remedy. The hold is announced once, as a
//! `provider_held` event the notify and signal plugins carry.

use crate::agent::{Outcome, Runner};
use crate::agent::refusal::{Probe, probe_login};
use crate::ctx::Forge;
use crate::report::Event;
use crate::unix_now;
use anyhow::Result;
use std::sync::OnceLock;

/// How often the worker re-probes a held provider.
pub const PROBE_EVERY_SECS: i64 = 600;

/// What the operator runs to log `provider` in again.
fn login_command(f: &Forge, provider: &str) -> &'static str {
    match f.providers.get(provider).map(|p| p.runner) {
        Some(Runner::CodexCli) => "codex login",
        Some(Runner::CopilotCli) => "copilot login",
        _ => "claude login",
    }
}

/// `epoch` as local wall-clock `HH:MM`: the hold is read by the operator
/// on the machine the worker runs on.
fn local_hhmm(epoch: i64) -> String {
    // SAFETY: localtime_r writes only into the tm this call owns.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = epoch as libc::time_t;
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// The hold in the operator's words, without the provider's name:
/// "login expired since 23:48; run claude login as the operator, then
/// forge doctor".
pub fn words(f: &Forge, provider: &str, since: i64) -> String {
    format!(
        "login expired since {}; run {} as the operator, then forge doctor",
        local_hhmm(since),
        login_command(f, provider)
    )
}

/// Hold `provider` for the login refusal `task_id`'s attempt met. The
/// first refusal starts the hold and announces it; later ones, from tasks
/// already running when it began, only find it held.
pub fn hold(f: &Forge, provider: &str, out: &Outcome, task_id: i64) -> Result<()> {
    let now = unix_now();
    let said = [out.result_text.as_str(), out.stderr_text.as_str()]
        .into_iter()
        .flat_map(str::lines)
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("the provider refused the agent login");
    if f.store.hold_login(provider, said, now)? {
        let reason = format!("{provider}: {}", words(f, provider, now));
        f.report.emit(
            task_id,
            Event::ProviderHeld {
                provider,
                reason: &reason,
                since: now,
            },
        );
    }
    Ok(())
}

/// `provider`'s login hold, as `worker::window_hold` reports a hold: the
/// message, and the second the next probe is due (when a waiting worker
/// should look again).
pub fn held(f: &Forge, provider: &str) -> Result<Option<(String, i64)>> {
    Ok(f.store.login_hold(provider)?.map(|h| {
        let last = h.probed_at.unwrap_or(h.since).max(h.since);
        (
            format!("{provider}: {}", words(f, provider, h.since)),
            (last + PROBE_EVERY_SECS).max(unix_now() + 60),
        )
    }))
}

/// Records one probe of `provider` and, when it answered, ends the hold
/// and announces that. True when the hold ended.
fn settle(f: &Forge, provider: &str, probe: &Probe) -> Result<bool> {
    f.store
        .record_probe(provider, probe.ok, probe.cost_usd, &probe.detail, unix_now())?;
    let released = probe.ok && f.store.release_login(provider)?;
    if released {
        f.report.emit(0, Event::ProviderReleased { provider });
    }
    Ok(released)
}

/// Where a probe runs: a scratch directory of its own, since the probe is
/// given nothing to work on.
fn probe_dir(f: &Forge) -> std::path::PathBuf {
    let dir = f.paths.home.join("tmp").join("login-probe");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The worker's probe, once per pass: each held provider not yet probed by
/// this process, or last probed ten minutes ago, is asked for one token.
pub async fn probe_due(f: &Forge) {
    static STARTED: OnceLock<i64> = OnceLock::new();
    let started = *STARTED.get_or_init(unix_now);
    for h in f.store.login_holds().unwrap_or_default() {
        let last = h.probed_at.unwrap_or(h.since).max(h.since);
        if last >= started && unix_now() - last < PROBE_EVERY_SECS {
            continue;
        }
        let Some(provider) = f.providers.get(&h.provider).cloned() else {
            continue;
        };
        let dir = probe_dir(f);
        let probe = tokio::task::spawn_blocking(move || probe_login(&provider, &dir))
            .await
            .unwrap_or_default();
        match settle(f, &h.provider, &probe) {
            Ok(true) => eprintln!("{}: the login answered a probe; released", h.provider),
            Ok(false) => eprintln!(
                "{}: still held, the probe was refused: {}",
                h.provider, probe.detail
            ),
            Err(e) => eprintln!("{}: recording the login probe: {e:#}", h.provider),
        }
    }
}

/// `forge doctor`'s probe: every held provider, now, blocking. What each
/// probe found, by provider.
pub fn probe_now(f: &Forge) -> Vec<(String, Probe)> {
    let mut found = Vec::new();
    for h in f.store.login_holds().unwrap_or_default() {
        let Some(provider) = f.providers.get(&h.provider) else {
            continue;
        };
        let probe = probe_login(provider, &probe_dir(f));
        let _ = settle(f, &h.provider, &probe);
        found.push((h.provider, probe));
    }
    found
}
