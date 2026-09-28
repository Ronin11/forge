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

/// Whether `text`, a runner's own error words, says its login was refused:
/// claude's "Failed to authenticate: OAuth session expired and could not be
/// refreshed" (2026-09-26, 40 attempts spent on it), codex's "unexpected
/// status 401 Unauthorized" or a refresh token it could not use, copilot's
/// "No authentication information found", or any HTTP 401/403 an API error
/// names. None of these is the agent's fault, and no retry fixes them: a
/// person has to log in.
pub fn login_failure(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    const SAID: &[&str] = &[
        "failed to authenticate",
        "authentication_error",
        "authentication failed",
        "oauth session expired",
        "oauth token has expired",
        "oauth token has been revoked",
        "could not be refreshed",
        "refresh token was already used",
        "invalid api key",
        "not logged in",
        "no authentication information",
        "please run /login",
        "please log in again",
    ];
    let status = t
        .split(|c: char| !c.is_ascii_digit())
        .any(|n| n == "401" || n == "403");
    SAID.iter().any(|s| t.contains(s))
        || status
            && ["api error", "unauthorized", "forbidden", "status"]
                .iter()
                .any(|w| t.contains(w))
}

/// A refused login: refunded like a spent window (`rate_limited`), and held
/// on no clock at all, until a probe answers (`crate::login_hold`).
pub fn login_refused(out: &mut Outcome) {
    out.rate_limited = true;
    out.login_refused = true;
}

/// The refusals a claude result frame with `is_error` names in its text: a
/// refused login, else a rate limit.
pub(super) fn read_claude_error(out: &mut Outcome, v: &Value) {
    if !out.is_error {
        return;
    }
    let text = v["result"].as_str().unwrap_or("");
    let lower = text.to_ascii_lowercase();
    if login_failure(text) {
        login_refused(out);
    } else if lower.contains("rate limit") || lower.contains("rate-limit") {
        hold_text_only(out);
    }
}

/// A run that failed with no answer and a refused login on stderr alone
/// (a CLI that never got as far as its event stream) is the same refusal.
pub(super) fn read_stderr(out: Result<Outcome>) -> Result<Outcome> {
    out.map(|mut o| {
        let answered = o.got_result && !o.is_error;
        if !answered && !o.rate_limited && o.exit_code != Some(0) && login_failure(&o.stderr_text) {
            login_refused(&mut o);
        }
        o
    })
}

/// What a login probe found: whether the login answered, what the one
/// token cost, and in a few words what the provider said.
#[derive(Debug, Default)]
pub struct Probe {
    pub ok: bool,
    pub cost_usd: f64,
    pub detail: String,
}

const PROBE_PROMPT: &str = "Reply with one word: ok";
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// `provider`'s CLI asked for one word on the host, in `dir` (a scratch
/// directory: the probe is given no tools and nothing to work on).
fn login_probe_argv(provider: &Provider, dir: &Path) -> Option<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let mut argv = match provider.runner {
        Runner::ClaudeCli => {
            let mut a = s(&[
                &super::real_bin(&super::agent_bin()),
                "--print",
                "--verbose",
            ]);
            a.extend(s(&[
                "--output-format",
                "stream-json",
                "--strict-mcp-config",
            ]));
            a.extend(s(&[
                "--disable-slash-commands",
                "--setting-sources",
                "project,local",
            ]));
            a.extend(s(&[
                "--max-turns",
                "1",
                "--tools",
                "",
                "--no-session-persistence",
            ]));
            a
        }
        Runner::CodexCli => {
            let mut a = s(&[&super::real_bin(&super::codex_bin()), "exec"]);
            a.extend(s(&[
                "--skip-git-repo-check",
                "--json",
                "-s",
                "read-only",
                "-C",
            ]));
            a.push(dir.display().to_string());
            a
        }
        Runner::CopilotCli => {
            let mut a = s(&[&super::real_bin(&super::copilot_bin()), "--output-format"]);
            a.extend(s(&[
                "json",
                "--disable-builtin-mcps",
                "--no-auto-update",
                "-C",
            ]));
            a.push(dir.display().to_string());
            a
        }
        Runner::Chat | Runner::Jev => return None,
    };
    if let Some(m) = provider.model.as_deref().filter(|m| !m.is_empty()) {
        let flag = if provider.runner == Runner::CodexCli {
            "-m"
        } else {
            "--model"
        };
        argv.extend(s(&[flag, m]));
    }
    argv.extend(provider.extra_args.iter().cloned());
    match provider.runner {
        Runner::CodexCli => argv.push(PROBE_PROMPT.to_string()),
        Runner::CopilotCli => argv.extend(s(&["-p", PROBE_PROMPT])),
        _ => {}
    }
    Some(argv)
}

/// Probe `provider`'s login with a one-token request on the host, blocking
/// for at most two minutes: `ok` when the CLI answered without an error.
/// A runner with no login of its own (chat, jev) always answers.
pub fn probe_login(provider: &Provider, dir: &Path) -> Probe {
    let Some(argv) = login_probe_argv(provider, dir) else {
        return Probe {
            ok: true,
            ..Probe::default()
        };
    };
    let extra = super::inputs::provider_env(provider);
    let mut spawned = Err(std::io::Error::other("never spawned"));
    for _ in 0..20 {
        let mut cmd = super::command_in(None, dir, &argv, &extra);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        spawned = cmd.spawn();
        match &spawned {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => break,
        }
    }
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            return Probe {
                detail: format!("cannot start {}: {e}", argv[0]),
                ..Probe::default()
            };
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut stdin, PROBE_PROMPT.as_bytes());
    }
    let pid = child.id() as libc::pid_t;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let Ok(Ok(output)) = rx.recv_timeout(PROBE_TIMEOUT) else {
        // SAFETY: kill(2) on the pid of the child this probe spawned.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        return Probe {
            detail: "the probe did not answer in two minutes".into(),
            ..Probe::default()
        };
    };
    read_probe(
        provider,
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )
}

/// A probe's output read with the runner's own parser.
fn read_probe(provider: &Provider, exited_ok: bool, stdout: &str, stderr: &str) -> Probe {
    let mut out = Outcome::default();
    let mut watch = Watch::new(crate::config::EarlyEnding {
        no_edit_calls: 0,
        edits_without_commit: 0,
        repeats: 0,
        signals_to_end: 0,
    });
    let mut tally = super::CopilotTally::default();
    let mut cost = None;
    for v in stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        match provider.runner {
            Runner::ClaudeCli if v["type"] == "result" => {
                out.got_result = true;
                out.is_error = v["is_error"].as_bool().unwrap_or(false);
                out.result_text = v["result"].as_str().unwrap_or("").to_string();
                cost = v["total_cost_usd"].as_f64();
                read_claude_error(&mut out, &v);
            }
            Runner::CodexCli => {
                super::apply_codex_event(&v, &mut out, &mut watch, false);
            }
            Runner::CopilotCli => {
                super::apply_copilot_event(&v, &mut out, &mut watch, false, &mut tally);
            }
            _ => {}
        }
    }
    let per_m = |n: Option<i64>, price: f64| n.unwrap_or(0) as f64 * price / 1e6;
    let cost_usd = cost.unwrap_or_else(|| {
        per_m(out.input_tokens, provider.price_input_per_million)
            + per_m(out.output_tokens, provider.price_output_per_million)
            + tally.premium_requests as f64 * provider.price_per_request
    });
    let ok = exited_ok && out.got_result && !out.is_error && !out.rate_limited;
    let said = [out.result_text.as_str(), stderr]
        .iter()
        .flat_map(|t| t.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    Probe {
        ok,
        cost_usd,
        detail: if ok {
            "answered".to_string()
        } else {
            super::truncated_first_line(&said)
        },
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
    let out = read_stderr(super::run_claude(l).await);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_login_in_any_runners_words_is_a_login_failure() {
        for text in [
            "Failed to authenticate: OAuth session expired and could not be refreshed",
            "API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}",
            "API Error: 403 Forbidden",
            "unexpected status 401 Unauthorized: Missing bearer",
            "Your access token could not be refreshed because your refresh token was already used",
            "Error: No authentication information found.",
        ] {
            assert!(login_failure(text), "{text}");
        }
        for text in [
            "You have hit your rate limit",
            "tests expect a 401 on a bad token; 12 passed",
            "error_max_turns",
            "",
        ] {
            assert!(!login_failure(text), "{text}");
        }
    }

    #[test]
    fn the_claude_frame_of_2026_09_26_is_a_refused_login_not_a_window() {
        let v: Value = serde_json::from_str(
            r#"{"type":"result","is_error":true,"terminal_reason":"api_error","result":"Failed to authenticate: OAuth session expired and could not be refreshed"}"#,
        )
        .unwrap();
        let mut out = Outcome {
            is_error: true,
            ..Default::default()
        };
        read_claude_error(&mut out, &v);
        assert!(out.rate_limited && out.login_refused);
        assert!(out.rate_limits.five_hour.is_none(), "no clock on it");
    }

    #[test]
    fn a_refused_login_only_on_stderr_is_read_there_and_an_answer_never_is() {
        let failed = |exit, got_result| Outcome {
            exit_code: Some(exit),
            got_result,
            stderr_text: "Error: unexpected status 401 Unauthorized".into(),
            ..Default::default()
        };
        assert!(read_stderr(Ok(failed(1, false))).unwrap().login_refused);
        assert!(!read_stderr(Ok(failed(0, true))).unwrap().login_refused);
    }

    fn claude() -> Provider {
        Provider::default()
    }

    #[test]
    fn a_login_probe_reads_the_refusal_and_the_answer_with_the_runners_parser() {
        let refused = r#"{"type":"result","is_error":true,"terminal_reason":"api_error","total_cost_usd":0,"result":"Failed to authenticate: OAuth session expired and could not be refreshed"}"#;
        let p = read_probe(&claude(), false, refused, "");
        assert!(!p.ok);
        assert!(p.detail.contains("OAuth session expired"), "{}", p.detail);
        let answered =
            r#"{"type":"result","is_error":false,"total_cost_usd":0.0004,"result":"ok"}"#;
        let p = read_probe(&claude(), true, answered, "");
        assert!(p.ok && (p.cost_usd - 0.0004).abs() < 1e-12, "{p:?}");
        let codex = Provider {
            runner: Runner::CodexCli,
            ..Provider::default()
        };
        let failed =
            r#"{"type":"turn.failed","error":{"message":"unexpected status 401 Unauthorized"}}"#;
        assert!(!read_probe(&codex, false, failed, "").ok);
        let said = r#"{"type":"item.completed","item":{"type":"agent_message","text":"ok"}}"#;
        assert!(read_probe(&codex, true, said, "").ok);
    }

    #[test]
    fn the_probe_is_the_attempt_argv_cut_to_one_toolless_unsaved_turn() {
        let s = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let argv = probe_argv(s(&[
            "claude",
            "--print",
            "--max-turns",
            "40",
            "--json-schema",
            "{}",
            "--tools",
            "Bash,Read",
            "--resume",
            "abc",
            "--model",
            "sonnet",
        ]));
        assert_eq!(
            argv,
            s(&[
                "claude",
                "--print",
                "--max-turns",
                "1",
                "--tools",
                "",
                "--model",
                "sonnet",
                "--no-session-persistence"
            ])
        );
    }
}
