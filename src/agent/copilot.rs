//! The copilot runner: `copilot` in two phases, the work and then the
//! structured report, its JSON frames folded into the outcome.

use super::*;

/// Phase two's fixed prompt for the copilot runner, the schema appended:
/// copilot has no schema flag at all, so the shape the answer must take is
/// quoted in the prompt itself and the envelope read out of the final
/// message (see `json_in_message`).
pub(super) const COPILOT_REPORT_PROMPT: &str = "Do no further work. Report the structured \
result for everything done in this session so far: one JSON object matching \
the JSON schema below exactly, with no prose before or after it and no code \
fence.\n\nSchema:\n";

/// The copilot CLI's argv for one phase, with the prompt on stdin: the
/// JSON event stream; every tool and path auto-approved (non-interactive
/// mode refuses to run without it; the sandbox, not the CLI's prompt, is
/// the boundary, as for the other runners); the bundled GitHub MCP server
/// off (an attempt gets the repository's tools and no other); no
/// self-update; its own logs at error level; `-C` the worktree; `--model`
/// when the launch names one (absent, the CLI's own choice); the provider's
/// own args; `--resume <session>` to continue one.
fn copilot_argv(bin: &str, l: &Launch<'_>, resume: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        bin.to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "--allow-all-tools".to_string(),
        "--allow-all-paths".to_string(),
        "--disable-builtin-mcps".to_string(),
        "--no-auto-update".to_string(),
        "--log-level".to_string(),
        "error".to_string(),
        "-C".to_string(),
        l.worktree.display().to_string(),
    ];
    if !l.model.is_empty() {
        argv.push("--model".to_string());
        argv.push(l.model.to_string());
    }
    argv.extend(l.provider.extra_args.iter().cloned());
    if let Some(id) = resume {
        argv.push("--resume".to_string());
        argv.push(id.to_string());
    }
    argv
}

/// The copilot tool a frame names, in the vocabulary `Watch` counts: its
/// shell tool is a `Bash` call carrying the command, its file-writing
/// tools are edits, anything else is a call and no more.
fn copilot_tool_for_watch(name: &str, args: &Value) -> (&'static str, Value) {
    match name {
        "bash" | "shell" => (
            "Bash",
            serde_json::json!({ "command": args["command"].as_str().unwrap_or("") }),
        ),
        "edit" | "create" | "write" | "str_replace_editor" | "apply_patch" => ("Edit", Value::Null),
        _ => ("Other", Value::Null),
    }
}

/// The JSON object a final message carries, when it is one: the text as
/// given, or the body of a ```json fence around it, provided it parses as a
/// JSON object. `None` for prose.
fn json_in_message(text: &str) -> Option<String> {
    let mut body = text.trim();
    if let Some(rest) = body.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        body = rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .map(|_| body.to_string())
}

/// What the copilot stream counts that `Outcome` does not carry itself:
/// the tool calls already counted (a request is announced on its
/// `assistant.message` and again as `tool.execution_start`), and the
/// premium requests the CLI's `result` frames report, which are what a
/// plan meters (see `run_copilot`).
#[derive(Default)]
pub(super) struct CopilotTally {
    seen_tools: HashSet<String>,
    pub(super) premium_requests: i64,
}

/// One frame of the copilot CLI's `--output-format json` stream folded into
/// `out` and `watch`: a `tool.execution_start` is a tool call (fed to the
/// early-ending watch), a `model.call_finished` a turn, an `assistant.usage`
/// frame's tokens are summed when the CLI sends one (1.0.88 sends none:
/// only premium requests, on `result`), an `assistant.message` with
/// content is the answer so far (the last wins: phase one's prose, then
/// phase two's envelope), a `session.error` a failure unless an answer
/// follows — and a refusal for a spent plan, which holds the provider an
/// hour — and `result` names the session to resume. Returns the
/// early-ending text when a tool call trips enough signals.
pub(super) fn apply_copilot_event(
    v: &Value,
    out: &mut Outcome,
    watch: &mut Watch,
    writes: bool,
    tally: &mut CopilotTally,
) -> Option<String> {
    let d = &v["data"];
    match v["type"].as_str() {
        Some("tool.execution_start") => {
            let id = d["toolCallId"].as_str().unwrap_or("");
            if !id.is_empty() && !tally.seen_tools.insert(id.to_string()) {
                return None;
            }
            out.tool_calls += 1;
            let (name, input) =
                copilot_tool_for_watch(d["toolName"].as_str().unwrap_or(""), &d["arguments"]);
            watch.saw(name, &input);
            if let Some(tripped) = watch.should_end(writes) {
                let text = tripped
                    .iter()
                    .map(|(_, w)| w.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                out.ended_early = Some(text.clone());
                return Some(text);
            }
        }
        Some("model.call_finished") => out.num_turns += 1,
        Some("assistant.usage") => {
            let add = |slot: &mut Option<i64>, key: &str| {
                if let Some(n) = d[key].as_i64() {
                    *slot = Some(slot.unwrap_or(0) + n);
                }
            };
            add(&mut out.input_tokens, "inputTokens");
            add(&mut out.output_tokens, "outputTokens");
            add(&mut out.cache_read_input_tokens, "cacheReadTokens");
            add(&mut out.cache_creation_input_tokens, "cacheWriteTokens");
        }
        Some("assistant.message") => {
            let content = d["content"].as_str().unwrap_or("");
            if !content.trim().is_empty() {
                out.got_result = true;
                out.is_error = false;
                out.structured = json_in_message(content);
                out.result_text = content.to_string();
            }
        }
        Some("session.error") => {
            let msg = d["message"].as_str().unwrap_or("").to_ascii_lowercase();
            out.is_error = true;
            if ["rate limit", "usage limit", "quota"]
                .iter()
                .any(|s| msg.contains(s))
            {
                out.rate_limited = true;
                out.rate_limits.five_hour = Some((1.0, crate::unix_now() + 3600));
            } else if refusal::login_failure(&msg) {
                refusal::login_refused(out);
            }
        }
        Some("result") => {
            if let Some(id) = v["sessionId"].as_str() {
                out.session_id = Some(id.to_string());
            }
            tally.premium_requests += v["usage"]["premiumRequests"].as_i64().unwrap_or(0);
        }
        _ => {}
    }
    None
}

/// A copilot phase: `run_json_phase` with the copilot frame parser, each
/// tool call reported as it starts.
async fn run_copilot_phase(args: RunCopilotPhase<'_>) -> Result<(Option<i32>, bool, String)> {
    let RunCopilotPhase {
        l,
        argv,
        prompt,
        extra_env,
        start,
        log,
        out,
        watch,
        tally,
    } = args;
    let (report, task_id, writes) = (l.report, l.task_id, l.writes);
    let mut apply = |v: &Value, out: &mut Outcome, watch: &mut Watch| {
        if v["type"] == "tool.execution_start" {
            let name = v["data"]["toolName"].as_str().unwrap_or("tool");
            report.emit(task_id, Event::ToolCall { name });
        }
        apply_copilot_event(v, out, watch, writes, tally)
    };
    run_json_phase(RunJsonPhase {
        l,
        argv,
        prompt,
        extra_env,
        start,
        log,
        out,
        watch,
        apply: &mut apply,
    })
    .await
}

/// The copilot-cli backend (GitHub Copilot CLI, `copilot`), run in the
/// same two phases as codex and for the same reason, with one difference:
/// copilot has no schema flag, so phase two quotes the schema in its prompt
/// and the envelope is read out of the final message. Phase one runs the
/// prompt (or resumes the attempt's session); once it ends, phase two
/// resumes that session with `COPILOT_REPORT_PROMPT`. Both phases write the prompt to stdin
/// and close it at EOF, selecting non-interactive execution. The CLI meters premium requests, not
/// tokens (1.0.88 reports no token counts at all), so the attempt's cost is
/// the requests its `result` frames report at the provider's
/// `price_usd_per_premium_request` — 0 within a plan's allowance — plus
/// whatever tokens it does report at the per-million prices.
pub(super) async fn run_copilot(l: Launch<'_>) -> Result<Outcome> {
    let bin = crate::executor::agent_bin(l.sandbox, l.worktree, copilot_bin_for(l.step));
    let mut extra_env = l.identity.clone();
    extra_env.extend(inputs::provider_env(l.provider));
    extra_env.push(("COPILOT_AUTO_UPDATE".to_string(), "false".to_string()));

    let mut log = CappedLog::create(l.log_path)
        .with_context(|| format!("creating {}", l.log_path.display()))?;
    writeln!(
        log,
        "{{\"type\":\"forge_prompt\",\"text\":{}}}",
        serde_json::to_string(l.prompt)?
    )?;

    let start = Instant::now();
    let mut out = Outcome {
        session_id: l.resume.map(|s| s.to_string()),
        ..Outcome::default()
    };
    let mut watch = Watch::new(l.early_ending);
    let mut tally = CopilotTally::default();

    let argv1 = copilot_argv(&bin, &l, l.resume);
    let (exit1, timed_out1, mut stderr_text) = run_copilot_phase(RunCopilotPhase {
        l: &l,
        argv: &argv1,
        prompt: l.prompt,
        extra_env: &extra_env,
        start: &start,
        log: &mut log,
        out: &mut out,
        watch: &mut watch,
        tally: &mut tally,
    })
    .await?;
    out.exit_code = exit1;
    out.timed_out = timed_out1;

    // Ask the same session to report itself structurally — only when there
    // is one to resume; a run that never reached its `result` frame has
    // nothing for phase two to continue.
    if let Some(session) = out.session_id.clone() {
        writeln!(
            log,
            "{{\"type\":\"forge_phase_two\",\"forge_ms\":{},\"session_id\":{}}}",
            start.elapsed().as_millis(),
            serde_json::to_string(&session)?
        )?;
        l.report.emit(
            l.task_id,
            Event::Note {
                text: "phase 2  resuming the session for the structured report",
            },
        );
        let prompt2 = format!("{COPILOT_REPORT_PROMPT}{}", l.schema);
        let argv2 = copilot_argv(&bin, &l, Some(&session));
        let (exit2, timed_out2, stderr2) = run_copilot_phase(RunCopilotPhase {
            l: &l,
            argv: &argv2,
            prompt: &prompt2,
            extra_env: &extra_env,
            start: &start,
            log: &mut log,
            out: &mut out,
            watch: &mut watch,
            tally: &mut tally,
        })
        .await?;
        out.exit_code = exit2;
        out.timed_out = out.timed_out || timed_out2;
        stderr_text.push_str(&stderr2);
    }

    out.early_signals = watch.tripped(l.writes).iter().map(|(k, _)| *k).collect();
    out.early_near = watch.near(l.writes);
    if !out.is_error {
        out.is_error = out.exit_code.is_some_and(|c| c != 0);
    }
    out.wall_ms = start.elapsed().as_millis();

    let mut cost = tally.premium_requests as f64 * l.provider.price_per_request;
    if let (Some(input), Some(output)) = (out.input_tokens, out.output_tokens) {
        cost += input as f64 * l.provider.price_input_per_million / 1_000_000.0
            + output as f64 * l.provider.price_output_per_million / 1_000_000.0;
    }
    out.cost_usd = Some(cost);
    writeln!(
        log,
        "{{\"type\":\"forge_copilot_usage\",\"premium_requests\":{}}}",
        tally.premium_requests
    )?;

    if !stderr_text.trim().is_empty() {
        writeln!(
            log,
            "{{\"type\":\"forge_stderr\",\"text\":{}}}",
            serde_json::to_string(&stderr_text)?
        )?;
    }
    out.stderr_text = stderr_text;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{test_launch, thresholds};
    use super::*;
    use crate::config::EarlyEnding;

    /// Captured frames from a `copilot -p --output-format json` run
    /// (1.0.88): a message announcing a tool request, the same call
    /// starting and completing, two model calls, a plain final message, and
    /// the result frame naming the session and the premium requests spent.
    fn copilot_fixture() -> Vec<&'static str> {
        vec![
            r#"{"type":"assistant.message","data":{"content":"","toolRequests":[{"toolCallId":"call_0","name":"bash","arguments":{"command":"echo 42 > answer.txt"}}]}}"#,
            r#"{"type":"tool.execution_start","data":{"toolCallId":"call_0","toolName":"bash","arguments":{"command":"echo 42 > answer.txt"}}}"#,
            r#"{"type":"tool.execution_complete","data":{"toolCallId":"call_0","success":true}}"#,
            r#"{"type":"model.call_finished","data":{"turnId":"0"}}"#,
            r#"{"type":"assistant.message","data":{"content":"wrote 42 to answer.txt","toolRequests":[]}}"#,
            r#"{"type":"model.call_finished","data":{"turnId":"1"}}"#,
            r#"{"type":"result","sessionId":"copilot-sess-1","exitCode":0,"usage":{"premiumRequests":2}}"#,
        ]
    }

    fn run_copilot_fixture(lines: &[&str], thresholds: EarlyEnding) -> (Outcome, CopilotTally) {
        let mut out = Outcome::default();
        let mut watch = Watch::new(thresholds);
        let mut tally = CopilotTally::default();
        for line in lines {
            let v: Value = serde_json::from_str(line).unwrap();
            if let Some(text) = apply_copilot_event(&v, &mut out, &mut watch, true, &mut tally) {
                out.ended_early = Some(text);
                break;
            }
        }
        (out, tally)
    }

    #[test]
    fn copilot_events_parse_into_the_outcome() {
        let (out, tally) = run_copilot_fixture(&copilot_fixture(), thresholds(100, 100, 100, 2));
        assert_eq!(out.session_id.as_deref(), Some("copilot-sess-1"));
        assert_eq!(
            out.tool_calls, 1,
            "the request on the message and its execution_start are one call"
        );
        assert_eq!(out.num_turns, 2, "one per model call");
        assert!(out.got_result && !out.is_error);
        assert_eq!(out.result_text, "wrote 42 to answer.txt");
        assert!(out.structured.is_none(), "prose is not a structured result");
        assert_eq!(tally.premium_requests, 2);
        assert_eq!(
            out.input_tokens, None,
            "1.0.88 reports no tokens, and none are invented"
        );
    }

    #[test]
    fn a_phase_two_copilot_message_carries_the_envelope_fenced_or_not() {
        let mut lines = copilot_fixture();
        lines.push(
            r#"{"type":"assistant.message","data":{"content":"```json\n{\"schema_version\":1,\"summary\":\"wrote 42\",\"needs_input\":null,\"changes\":[],\"checks_run\":[],\"claims\":[]}\n```","toolRequests":[]}}"#,
        );
        lines.push(
            r#"{"type":"result","sessionId":"copilot-sess-1","exitCode":0,"usage":{"premiumRequests":1}}"#,
        );
        let (out, tally) = run_copilot_fixture(&lines, thresholds(100, 100, 100, 2));
        let structured: Value =
            serde_json::from_str(&out.structured.expect("phase two's envelope")).unwrap();
        assert_eq!(structured["summary"], "wrote 42");
        assert_eq!(
            tally.premium_requests, 3,
            "both phases' requests count into the one attempt"
        );
        assert_eq!(json_in_message("{\"a\":1}").as_deref(), Some("{\"a\":1}"));
        assert_eq!(
            json_in_message("```\n{\"a\":1}\n```").as_deref(),
            Some("{\"a\":1}")
        );
        assert_eq!(
            json_in_message("[1,2]"),
            None,
            "an array is not the envelope"
        );
        assert_eq!(json_in_message("done"), None);
    }

    #[test]
    fn a_copilot_session_error_with_no_answer_is_a_failure_and_a_spent_plan_a_refusal() {
        let lines = [
            r#"{"type":"session.error","data":{"message":"stream disconnected"}}"#,
            r#"{"type":"result","sessionId":"copilot-sess-2","exitCode":1,"usage":{"premiumRequests":0}}"#,
        ];
        let (out, _) = run_copilot_fixture(&lines, thresholds(100, 100, 100, 2));
        assert!(out.is_error && !out.got_result && !out.rate_limited);

        let lines = [
            r#"{"type":"session.error","data":{"message":"You have exceeded your premium request quota"}}"#,
        ];
        let (out, _) = run_copilot_fixture(&lines, thresholds(100, 100, 100, 2));
        assert!(out.is_error && out.rate_limited);
        let (utilization, reset) = out.rate_limits.five_hour.unwrap();
        assert_eq!(utilization, 1.0);
        assert!(reset > crate::unix_now() + 3000, "held about an hour");
    }

    #[test]
    fn copilot_tool_calls_feed_the_early_ending_watch() {
        let lines = [
            r#"{"type":"tool.execution_start","data":{"toolCallId":"c1","toolName":"bash","arguments":{"command":"grep foo"}}}"#,
            r#"{"type":"tool.execution_start","data":{"toolCallId":"c2","toolName":"bash","arguments":{"command":"grep foo"}}}"#,
            r#"{"type":"assistant.message","data":{"content":"never reached","toolRequests":[]}}"#,
        ];
        let (out, _) = run_copilot_fixture(&lines, thresholds(100, 100, 2, 1));
        assert_eq!(
            out.ended_early.as_deref(),
            Some("`grep foo` run 2 times"),
            "the repeated command trips the same Watch the claude runner uses"
        );
        assert!(!out.got_result, "the run ended before the message");
    }

    #[test]
    fn copilot_argv_carries_stream_flags_model_resume_without_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let report = Reporter::new(false, None);
        let provider = Provider {
            runner: Runner::CopilotCli,
            extra_args: vec!["--reasoning-effort".into(), "high".into()],
            ..Provider::default()
        };
        let log_path = dir.path().join("log.jsonl");
        let l = test_launch(
            dir.path(),
            &report,
            &provider,
            &log_path,
            r#"{"type":"object"}"#,
            false,
            None,
        );
        let argv = copilot_argv("copilot", &l, Some("sess-1"));
        assert_eq!(argv[0], "copilot");
        for flag in [
            "--allow-all-tools",
            "--allow-all-paths",
            "--disable-builtin-mcps",
            "--no-auto-update",
        ] {
            assert!(argv.contains(&flag.to_string()), "{flag}: {argv:?}");
        }
        let at = |f: &str| argv.iter().position(|a| a == f).unwrap();
        assert_eq!(argv[at("--output-format") + 1], "json");
        assert_eq!(argv[at("-C") + 1], dir.path().display().to_string());
        assert_eq!(argv[at("--model") + 1], "sonnet");
        assert_eq!(argv[at("--reasoning-effort") + 1], "high");
        assert_eq!(argv[at("--resume") + 1], "sess-1");
        assert!(!argv.iter().any(|a| a == "-p" || a == l.prompt));
        assert!(
            !copilot_argv("copilot", &l, None).contains(&"--resume".to_string()),
            "no resume on a fresh session"
        );
    }
}
