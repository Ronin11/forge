//! A refusal recognized only from result text, with no `rate_limit_event`
//! sample: hold the provider for a short cool-down anyway, or the refund
//! leaves nothing to stop the directive relaunching at once, indefinitely
//! (docs/REVIEW-3.md #1.1.4).

use super::*;

pub fn hold_text_only(out: &mut Outcome) {
    out.rate_limited = true;
    if out.rate_limits.five_hour.is_none() {
        out.rate_limits.five_hour = Some((1.0, crate::unix_now() + 300));
    }
}

/// Whether a claude launch runs on the operator's subscription login: an API
/// key or a long-lived token in the environment stands in for it.
fn uses_login(l: &Launch<'_>) -> bool {
    let set = |k: &str| {
        std::env::var_os(k).is_some_and(|v| !v.is_empty())
            || super::inputs::provider_env(l.provider)
                .iter()
                .any(|(n, v)| n == k && !v.is_empty())
    };
    let keyed = [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ];
    !keyed.iter().any(|k| set(k))
        && l.sandbox
            .is_none_or(|e| e.backend(l.worktree) != crate::executor::Backend::Ssh)
}

/// The claude launch the kernel guards the login around (see `login`): a
/// host login with an empty token is a provider refusal, never an attempt; a
/// near-expiry one is refreshed on the host first; and whatever the attempt's
/// sandbox refreshed is written back over the host file after it.
pub(super) async fn guarded_claude(l: Launch<'_>) -> Result<Outcome> {
    let dir = crate::login::config_dir().filter(|_| uses_login(&l));
    if let Some(dir) = &dir
        && let Some(why) = login_problem(&l, dir).await
    {
        return refuse_login(&l, &why);
    }
    let (sandbox, worktree, report, id) = (l.sandbox, l.worktree, l.report, l.task_id);
    let out = super::run_claude(l).await;
    if dir.is_some_and(|_| sandbox.is_some_and(|sb| sb.write_back_login(worktree))) {
        let text = "login    a refreshed token was written back to the host file";
        report.emit(id, Event::Note { text });
    }
    out
}

/// Why the host login cannot start an attempt, after refreshing it if it is
/// near expiry; `None` when it can (or there is no file to speak of).
async fn login_problem(l: &Launch<'_>, dir: &Path) -> Option<String> {
    let state = match crate::login::host_state(dir) {
        crate::login::Host::Usable(c) if c.near_expiry(crate::unix_now() * 1000) => {
            refresh_on_host(l, dir).await
        }
        s => s,
    };
    (state == crate::login::Host::Empty).then(|| {
        format!(
            "the agent login in {} has an empty token; run `claude login`",
            dir.join(crate::login::FILE).display()
        )
    })
}

/// Refresh the login on the host, one launch at a time: hold the login's
/// lock, take any later pair a sandbox holds (its refresh token is the live
/// one; probing with the host's dead one would empty the file), and only if
/// the host file is still near expiry, run a one-token probe through the
/// attempts' own lean argv so the CLI refreshes it. The file as it stands.
async fn refresh_on_host(l: &Launch<'_>, dir: &Path) -> crate::login::Host {
    let owned = dir.to_path_buf();
    let _lock = tokio::task::spawn_blocking(move || crate::login::lock(&owned))
        .await
        .ok();
    for copy in crate::login::private_copies(l.worktree) {
        let _ = crate::login::write_back_locked(dir, &copy);
    }
    let state = crate::login::host_state(dir);
    match state {
        crate::login::Host::Usable(c) if c.near_expiry(crate::unix_now() * 1000) => {
            probe(l).await;
            crate::login::host_state(dir)
        }
        s => s,
    }
}

/// `argv` as a one-token probe: no tools, one turn, no schema, nothing saved.
fn probe_argv(mut argv: Vec<String>) -> Vec<String> {
    let mut drop_pair = |flag: &str, keep: Option<&str>| {
        if let Some(i) = argv.iter().position(|a| a == flag) {
            match keep {
                Some(v) => argv[i + 1] = v.to_string(),
                None => {
                    argv.drain(i..=i + 1);
                }
            }
        }
    };
    drop_pair("--max-turns", Some("1"));
    drop_pair("--tools", Some(""));
    drop_pair("--json-schema", None);
    drop_pair("--resume", None);
    argv.push("--no-session-persistence".to_string());
    argv
}

async fn probe(l: &Launch<'_>) {
    let bin = crate::executor::agent_bin(l.sandbox, l.worktree, super::agent_bin_for(l.step));
    let argv = probe_argv(super::claude_argv(&bin, l));
    let extra = super::inputs::provider_env(l.provider);
    let Ok(mut child) = super::spawn_retrying_etxtbsy(|| {
        let mut c =
            tokio::process::Command::from(super::command_in(None, l.worktree, &argv, &extra));
        c.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        c
    })
    .await
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut stdin, b"Reply with one word: ok").await;
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(120), child.wait()).await;
}

/// The refusal: no attempt is made and none is counted. It holds the
/// provider for a few minutes like any other refusal (so the worker does not
/// spin on it), and says what to do.
fn refuse_login(l: &Launch<'_>, why: &str) -> Result<Outcome> {
    let mut out = Outcome::default();
    hold_text_only(&mut out);
    out.exit_code = Some(1);
    out.stderr_text = why.to_string();
    let mut log = std::fs::File::create(l.log_path)
        .with_context(|| format!("creating {}", l.log_path.display()))?;
    writeln!(
        log,
        "{{\"type\":\"forge_prompt\",\"text\":{}}}",
        serde_json::to_string(l.prompt)?
    )?;
    writeln!(
        log,
        "{{\"type\":\"forge_stderr\",\"text\":{}}}",
        serde_json::to_string(why)?
    )?;
    l.report.emit(l.task_id, Event::Note { text: why });
    Ok(out)
}
