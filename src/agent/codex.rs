//! The codex runner: `codex exec` in two phases, the work (with nudges
//! when it stalls) and then the structured report, its events folded into
//! the outcome.

use super::*;

/// Applies one parsed line of codex's `--json` event stream to `out` and
/// `watch`; the side effects that need the log file or the reporter (every
/// line gets written verbatim, a command execution is reported as a tool
/// call) are the caller's, in `run_codex`, so this stays pure enough to
/// unit-test against captured lines. Returns the early-ending signals'
/// text when `Watch` says enough of them tripped to stop the run.
pub(super) fn apply_codex_event(
    v: &Value,
    out: &mut Outcome,
    watch: &mut Watch,
    writes: bool,
) -> Option<String> {
    match v["type"].as_str() {
        Some("thread.started") => {
            if let Some(id) = v["thread_id"].as_str() {
                out.session_id = Some(id.to_string());
            }
        }
        Some("item.started") => {
            if v["item"]["type"] == "command_execution" {
                out.tool_calls += 1;
                let cmd = v["item"]["command"].as_str().unwrap_or("").to_string();
                watch.saw("Bash", &serde_json::json!({ "command": cmd }));
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
        }
        Some("item.completed") => match v["item"]["type"].as_str() {
            // Codex reports warnings as error items too ("Model metadata for
            // `qwen3-coder:30b` not found. Defaulting to fallback metadata"),
            // before it goes on to work. An error item is a failure only if
            // no result follows; a result clears it (task 288 was failed for
            // a warning while its structured result was a valid question).
            Some("error") => out.is_error = true,
            Some("agent_message") => {
                let text = v["item"]["text"].as_str().unwrap_or("").to_string();
                out.got_result = true;
                out.is_error = false;
                out.structured = serde_json::from_str::<Value>(&text)
                    .ok()
                    .map(|_| text.clone());
                out.result_text = text;
            }
            _ => {}
        },
        // A failed turn, or a top-level error frame: codex reports a spent
        // usage window this way ("You've hit your usage limit ... try again
        // at 3:37 AM"), and a refused login. Each is a refusal, refunded:
        // the window held until the time it names, the login until it
        // answers a probe.
        Some("turn.failed") | Some("error") => {
            let msg = v["error"]["message"]
                .as_str()
                .or(v["message"].as_str())
                .unwrap_or("");
            out.is_error = true;
            if let Some(reset) = usage_limit_reset(msg, crate::unix_now()) {
                out.rate_limited = true;
                out.rate_limits.five_hour = Some((1.0, reset));
            } else if refusal::login_failure(msg) {
                refusal::login_refused(out);
            }
        }
        Some("turn.completed") => {
            out.num_turns += 1;
            let u = &v["usage"];
            let input = u["input_tokens"].as_i64().unwrap_or(0);
            let cached = u["cached_input_tokens"].as_i64().unwrap_or(0);
            let output = u["output_tokens"].as_i64().unwrap_or(0)
                + u["reasoning_output_tokens"].as_i64().unwrap_or(0);
            out.input_tokens = Some(out.input_tokens.unwrap_or(0) + input);
            out.cache_read_input_tokens = Some(out.cache_read_input_tokens.unwrap_or(0) + cached);
            out.output_tokens = Some(out.output_tokens.unwrap_or(0) + output);
        }
        _ => {}
    }
    None
}

/// codex's `exec` flags shared by both of the two phases below, after `exec
/// [resume <id>]` and before whatever differs (`--output-schema` and the
/// prompt): `--skip-git-repo-check --json -C <worktree> [-m <model>]`,
/// sandboxed with `-s workspace-write` when Forge's own sandbox is off, or
/// `--dangerously-bypass-approvals-and-sandbox` when the attempt already
/// runs inside one (bubblewrap) and codex's own would only be redundant.
fn codex_common_argv(l: &Launch<'_>) -> Vec<String> {
    let mut argv = vec![
        "--skip-git-repo-check".to_string(),
        "--json".to_string(),
        "-c".to_string(),
        "mcp_servers={}".to_string(),
        "-C".to_string(),
        l.worktree.display().to_string(),
    ];
    if l.sandbox
        .is_some_and(|s| s.backend(l.worktree) == crate::executor::Backend::Bwrap)
    {
        argv.push("--dangerously-bypass-approvals-and-sandbox".into());
    } else {
        argv.push("-s".into());
        argv.push("workspace-write".into());
    }
    if !l.model.is_empty() {
        argv.push("-m".into());
        argv.push(l.model.to_string());
    }
    argv
}

/// A nudge's fixed prompt for a phase one that made no edit at all: told
/// once, plainly, to do the work rather than end the turn with only a
/// description of it.
const CODEX_NUDGE_IMPLEMENT_PROMPT: &str = "You have not made any changes yet. \
Implement the task now: make the change in the worktree, then commit it. Do \
not just describe what you would do — do it, then stop.";

/// A nudge's fixed prompt for a phase one that edited but left the tree
/// dirty.
const CODEX_NUDGE_COMMIT_PROMPT: &str =
    "You have uncommitted changes in the worktree. Commit them now, then stop.";

/// Whether phase one's own result already carries a `needs_input` worth
/// stopping for. No schema was ever put in front of phase one, so this only
/// fires when the model happened to answer in the envelope's shape
/// unprompted; its `question` must be long enough to carry real content
/// (over 60 characters) and not merely ask whether it may proceed — every
/// task's preamble already says it may.
fn phase_one_needs_real_input(out: &Outcome) -> bool {
    let Some(structured) = &out.structured else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(structured) else {
        return false;
    };
    let Some(question) = v["needs_input"]["question"].as_str() else {
        return false;
    };
    let q = question.trim();
    if q.chars().count() <= 60 {
        return false;
    }
    let lower = q.to_ascii_lowercase();
    let asks_to_proceed = [
        "may i proceed",
        "should i proceed",
        "ok to proceed",
        "okay to proceed",
        "want me to proceed",
        "shall i continue",
        "should i continue",
        "may i continue",
    ]
    .iter()
    .any(|p| lower.contains(p));
    !asks_to_proceed
}

/// Phase two's fixed prompt: no schema was ever put in front of the model
/// while it worked, so this is the first it hears of the shape its answer
/// must take. Named fields match `envelope::SCHEMA` so the model has enough
/// to go on without having seen the schema itself.
pub(super) const CODEX_REPORT_PROMPT: &str = "Do no further work. Report the structured \
result for everything done in this thread so far: schema_version, summary, \
checks_run, claims, and needs_input if you stopped for a reason \
before finishing, matching the schema you were given exactly.";

/// A codex `exec` phase: `run_json_phase` with the codex frame parser, each
/// command execution reported as a tool call as it starts.
async fn run_codex_phase(args: RunCodexPhase<'_>) -> Result<(Option<i32>, bool, String)> {
    let RunCodexPhase {
        l,
        argv,
        prompt,
        extra_env,
        start,
        log,
        out,
        watch,
    } = args;
    let (report, task_id, writes) = (l.report, l.task_id, l.writes);
    let mut apply = |v: &Value, out: &mut Outcome, watch: &mut Watch| {
        if v["type"] == "item.started" && v["item"]["type"] == "command_execution" {
            let name = v["item"]["command"].as_str().unwrap_or("command_execution");
            report.emit(task_id, Event::ToolCall { name });
        }
        apply_codex_event(v, out, watch, writes)
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

/// The codex-cli backend, run in two phases. A weaker model asked to commit
/// to `--output-schema`'s shape before it has done anything just answers
/// with a description of what it would do instead of doing it (dev.home's
/// qwen3-coder:30b through codex-cli, tasks 293-297: one turn, zero tool
/// calls, a schema-shaped result, under `--output-schema`; three tool calls
/// and a real edit, the same prompt, without it). So phase one runs the
/// prompt with no schema attached, and only once that run ends — with or
/// without a plain final message — does phase two resume the same thread
/// with `--output-schema` and a short fixed prompt asking only for the
/// structured report `run_codex` parses as the attempt's result. Each phase passes `-`
/// as the prompt argument and writes the prompt to stdin, closing it at EOF.
pub(super) async fn run_codex(l: Launch<'_>) -> Result<Outcome> {
    let bin = crate::executor::agent_bin(l.sandbox, l.worktree, codex_bin_for(l.step));
    // The schema is text (`envelope::SCHEMA`), but codex takes a file, and
    // codex reads it inside the sandbox, where Forge's home is an empty
    // tmpfs. The worktree is the one directory bound read-write for the
    // attempt, and its `.git` is invisible to `git status`, so the file
    // lives there (tasks 274-286 exited at launch: "Failed to read output
    // schema file", written beside the log under FORGE_HOME).
    let schema_path = l
        .worktree
        .join(".git")
        .join(format!("forge-{}-schema.json", l.step));
    inputs::write_codex_schema(&schema_path, l.schema)?;

    let mut extra_env = l.identity.clone();
    extra_env.extend(inputs::provider_env(l.provider));
    extra_env.push((
        "FORGE_CODEX_CONFIG".into(),
        inputs::codex_config(l.provider, l.model)?,
    ));

    let mut log =
        File::create(l.log_path).with_context(|| format!("creating {}", l.log_path.display()))?;
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

    // Every `exec` option (--json, -C, the sandbox flag, -m, the provider's
    // own args) goes before the `resume` subcommand: codex rejects them
    // after it ("error: unexpected argument '-C' found", task 305).
    let mut argv1: Vec<String> = vec![bin.clone(), "exec".to_string()];
    argv1.extend(codex_common_argv(&l));
    argv1.extend(l.provider.extra_args.iter().cloned());
    if let Some(id) = l.resume {
        argv1.push("resume".into());
        argv1.push(id.to_string());
    }
    argv1.push("-".into());

    let (exit1, timed_out1, mut stderr_text) = run_codex_phase(RunCodexPhase {
        l: &l,
        argv: &argv1,
        prompt: l.prompt,
        extra_env: &extra_env,
        start: &start,
        log: &mut log,
        out: &mut out,
        watch: &mut watch,
    })
    .await?;
    out.exit_code = exit1;
    out.timed_out = timed_out1;

    // A weak model's phase one that made no real progress (dev.home's
    // qwen3-coder:30b, tasks 309/313: three or four files read, then a
    // closing message saying the code was analysed; task 310: edits left
    // uncommitted) gets nudged, resuming the same thread with no schema and
    // a fixed prompt to do the work and commit — up to `nudges` times, each
    // one fed through the same early-ending `Watch` phase one used. Gated
    // on `writes`: a read-only step (review, plan) is never told to
    // "implement the task now". A substantive `needs_input` already in
    // phase one's own result means the run is genuinely blocked, not just
    // quiet, so it is never nudged past.
    if l.writes && l.provider.nudges > 0 && !phase_one_needs_real_input(&out) {
        let mut n = 0u32;
        while n < l.provider.nudges {
            let Some(thread_id) = out.session_id.clone() else {
                break;
            };
            let dirty = !crate::git::dirty_paths(l.worktree)
                .await
                .unwrap_or_default()
                .is_empty();
            let head = crate::git::head(l.worktree).await.ok();
            let (reason, prompt) = if !dirty && head.as_deref() == Some(l.start_sha) {
                ("no-edit", CODEX_NUDGE_IMPLEMENT_PROMPT)
            } else if dirty {
                ("uncommitted", CODEX_NUDGE_COMMIT_PROMPT)
            } else {
                // Edited and committed: nothing left to nudge.
                break;
            };
            n += 1;
            writeln!(
                log,
                "{{\"type\":\"forge_nudge\",\"forge_ms\":{},\"n\":{n},\"reason\":{}}}",
                start.elapsed().as_millis(),
                serde_json::to_string(reason)?
            )?;
            l.report.emit(
                l.task_id,
                Event::Note {
                    text: &format!(
                        "nudge    {reason} ({n}/{}); resuming with a fixed prompt",
                        l.provider.nudges
                    ),
                },
            );

            let mut argv_n: Vec<String> = vec![bin.clone(), "exec".to_string()];
            argv_n.extend(codex_common_argv(&l));
            argv_n.extend(l.provider.extra_args.iter().cloned());
            argv_n.push("resume".into());
            argv_n.push(thread_id);
            argv_n.push("-".into());

            let (exit_n, timed_out_n, stderr_n) = run_codex_phase(RunCodexPhase {
                l: &l,
                argv: &argv_n,
                prompt,
                extra_env: &extra_env,
                start: &start,
                log: &mut log,
                out: &mut out,
                watch: &mut watch,
            })
            .await?;
            out.exit_code = exit_n;
            out.timed_out = out.timed_out || timed_out_n;
            stderr_text.push_str(&stderr_n);
            if out.ended_early.is_some() {
                break;
            }
        }
    }

    // Whatever phase one (or the last nudge) ended with, ask the same
    // thread to report itself
    // structurally now — but only when there is a thread to resume; a run
    // that never got as far as `thread.started` has nothing for phase two
    // to continue.
    if let Some(thread_id) = out.session_id.clone() {
        writeln!(
            log,
            "{{\"type\":\"forge_phase_two\",\"forge_ms\":{},\"thread_id\":{}}}",
            start.elapsed().as_millis(),
            serde_json::to_string(&thread_id)?
        )?;
        l.report.emit(
            l.task_id,
            Event::Note {
                text: "phase 2  resuming the thread for the structured report",
            },
        );

        let mut argv2: Vec<String> = vec![bin.clone(), "exec".to_string()];
        argv2.extend(codex_common_argv(&l));
        argv2.extend(l.provider.extra_args.iter().cloned());
        argv2.push("resume".into());
        argv2.push(thread_id);
        argv2.push("--output-schema".into());
        argv2.push(schema_path.display().to_string());
        argv2.push("-".into());

        let (exit2, timed_out2, stderr2) = run_codex_phase(RunCodexPhase {
            l: &l,
            argv: &argv2,
            prompt: CODEX_REPORT_PROMPT,
            extra_env: &extra_env,
            start: &start,
            log: &mut log,
            out: &mut out,
            watch: &mut watch,
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

    // Codex reports no cost of its own; the operator's per-provider price
    // table (0 for a local model) turns its token counts into one.
    if let (Some(input), Some(output)) = (out.input_tokens, out.output_tokens) {
        out.cost_usd = Some(
            input as f64 * l.provider.price_input_per_million / 1_000_000.0
                + output as f64 * l.provider.price_output_per_million / 1_000_000.0,
        );
    }

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
    use super::super::tests::thresholds;
    use super::*;
    use crate::config::EarlyEnding;
    use std::path::PathBuf;

    /// Captured lines from a codex `exec --json` run: a thread starting, one
    /// command execution, an error item, a final assistant message carrying
    /// the envelope as its text, and the turn's usage.
    fn codex_fixture() -> Vec<&'static str> {
        vec![
            r#"{"type":"thread.started","thread_id":"codex-sess-1"}"#,
            r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"echo 42 > answer.txt"}}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"echo 42 > answer.txt","exit_code":0}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"error","message":"a tool call failed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"{\"schema_version\":1,\"summary\":\"wrote 42\",\"needs_input\":null,\"changes\":[{\"path\":\"answer.txt\",\"kind\":\"added\"}],\"checks_run\":[],\"claims\":[]}"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":50,"reasoning_output_tokens":5}}"#,
        ]
    }

    fn run_codex_fixture(lines: &[&str], thresholds: EarlyEnding) -> Outcome {
        let mut out = Outcome::default();
        let mut watch = Watch::new(thresholds);
        for line in lines {
            let v: Value = serde_json::from_str(line).unwrap();
            if let Some(text) = apply_codex_event(&v, &mut out, &mut watch, true) {
                out.ended_early = Some(text);
                break;
            }
        }
        out
    }

    #[test]
    fn a_codex_error_item_with_no_result_is_a_failure() {
        let lines = [
            r#"{"type":"thread.started","thread_id":"codex-sess-2"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"error","message":"stream disconnected"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":5,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0}}"#,
        ];
        let out = run_codex_fixture(&lines, thresholds(100, 100, 100, 2));
        assert!(out.is_error, "no result followed the error item");
        assert!(!out.got_result);
    }

    #[test]
    fn codex_events_parse_into_the_outcome() {
        let out = run_codex_fixture(&codex_fixture(), thresholds(100, 100, 100, 2));
        assert_eq!(out.session_id.as_deref(), Some("codex-sess-1"));
        assert_eq!(out.tool_calls, 1);
        assert!(
            !out.is_error,
            "an error item before a valid result is a warning, not a failure"
        );
        assert!(out.got_result);
        assert_eq!(out.result_text, out.structured.clone().unwrap());
        let structured: Value = serde_json::from_str(&out.structured.unwrap()).unwrap();
        assert_eq!(structured["summary"], "wrote 42");
        assert_eq!(out.num_turns, 1);
        assert_eq!(out.input_tokens, Some(100));
        assert_eq!(out.cache_read_input_tokens, Some(10));
        // Reasoning tokens sum into the output count alongside the plain ones.
        assert_eq!(out.output_tokens, Some(55));
    }

    #[test]
    fn a_phase_one_stream_with_tool_calls_then_a_phase_two_structured_message_has_both() {
        // Phase one: the model works, ending with a plain final message that
        // is not itself JSON (no schema was ever put in front of it).
        let mut lines = vec![
            r#"{"type":"thread.started","thread_id":"codex-sess-1"}"#,
            r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"echo 42 > answer.txt"}}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"echo 42 > answer.txt","exit_code":0}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"wrote 42 to answer.txt"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":50,"reasoning_output_tokens":5}}"#,
        ];
        // Phase two: the resumed thread, asked only for the structured
        // report, answers with the envelope.
        lines.extend([
            r#"{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"{\"schema_version\":1,\"summary\":\"wrote 42\",\"needs_input\":null,\"changes\":[{\"path\":\"answer.txt\",\"kind\":\"added\"}],\"checks_run\":[],\"claims\":[]}"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":20,"cached_input_tokens":0,"output_tokens":8,"reasoning_output_tokens":0}}"#,
        ]);

        let out = run_codex_fixture(&lines, thresholds(100, 100, 100, 2));
        assert!(out.tool_calls > 0, "phase one's command execution counted");
        assert!(out.got_result);
        // Phase one's plain text is not JSON; only phase two's message is.
        let structured: Value =
            serde_json::from_str(&out.structured.expect("phase two's structured result")).unwrap();
        assert_eq!(structured["summary"], "wrote 42");
        // Both phases' turns and usage count into the one attempt.
        assert_eq!(out.num_turns, 2);
        assert_eq!(out.input_tokens, Some(120));
        assert_eq!(out.output_tokens, Some(63));
    }

    #[test]
    fn codex_command_executions_feed_the_early_ending_watch() {
        let lines = vec![
            r#"{"type":"thread.started","thread_id":"s"}"#,
            r#"{"type":"item.started","item":{"id":"i0","type":"command_execution","command":"grep foo"}}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"command_execution","command":"grep foo","exit_code":0}}"#,
            r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"grep foo"}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"grep foo","exit_code":0}}"#,
        ];
        let out = run_codex_fixture(&lines, thresholds(100, 100, 2, 1));
        assert_eq!(
            out.ended_early.as_deref(),
            Some("`grep foo` run 2 times"),
            "the repeated command trips the same Watch the claude runner uses"
        );
    }

    /// A codex fake told apart, like `codex-ok.sh`, by whether its argv
    /// carries `--output-schema` (phase two) or `resume` with no schema (a
    /// nudge): with neither, it plays phase one.
    fn write_fake(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("codex-fake.sh");
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn git(dir: &Path, args: &[&str]) {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success(),
            "git {args:?} in {}",
            dir.display()
        );
    }

    fn init_repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet"]);
        git(
            dir.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "base",
            ],
        );
        dir
    }

    fn nudge_provider(nudges: u32) -> Provider {
        Provider {
            runner: Runner::CodexCli,
            nudges,
            ..Provider::default()
        }
    }

    async fn run_codex_with_fake(
        dir: &Path,
        step: &str,
        script: &str,
        nudges: u32,
        start_sha: &str,
    ) -> (Outcome, String) {
        // The fake script itself lives outside the worktree Forge checks
        // `git status` against; the previous version of this helper put it
        // (and the log) inside the worktree, which made every dirty check
        // see it as an untracked file and nudge on every turn.
        let scratch = tempfile::tempdir().unwrap();
        let fake = write_fake(scratch.path(), script);
        let key = format!(
            "FORGE_CODEX_BIN_{}",
            step.to_ascii_uppercase().replace('-', "_")
        );
        // SAFETY: `step` (and so `key`) is unique to each test in this file,
        // so setting it process-wide races with nothing else that reads it.
        unsafe { std::env::set_var(&key, fake.to_string_lossy().to_string()) };
        let log_path = scratch.path().join("log.jsonl");
        let report = crate::report::Reporter::new(false, None);
        let provider = nudge_provider(nudges);
        let out = run_codex(Launch {
            task_id: 1,
            worktree: dir,
            identity: crate::git::identity(dir).await,
            prompt: "do the task",
            system: "",
            model: "fake-model",
            max_turns: 30,
            timeout: Duration::from_secs(5),
            check_timeout: Duration::ZERO,
            log_path: &log_path,
            sandbox: None,
            report: &report,
            step,
            provider: &provider,
            resume: None,
            writes: true,
            start_sha,
            schema: crate::envelope::SCHEMA,
            early_ending: thresholds(100, 100, 100, 2),
            no_tools: false,
            judgment: None,
        })
        .await
        .unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap();
        (out, log)
    }

    /// A phase one told apart by argv alone: `--output-schema` is phase
    /// two, a bare `resume` with no schema is a nudge, and neither is
    /// phase one itself.
    const NUDGE_FAKE_PHASE_TWO: &str = "\
if [ \"$has_schema\" = \"1\" ]; then\n\
  echo '{\"type\":\"item.completed\",\"item\":{\"id\":\"r\",\"type\":\"agent_message\",\"text\":\"{\\\"schema_version\\\":1,\\\"summary\\\":\\\"done\\\",\\\"needs_input\\\":null,\\\"changes\\\":[{\\\"path\\\":\\\"answer.txt\\\",\\\"kind\\\":\\\"added\\\"}],\\\"checks_run\\\":[],\\\"claims\\\":[]}\"}}'\n\
  echo '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}'\n";

    const NUDGE_FAKE_HEADER: &str = "#!/bin/sh\n\
has_schema=0\n\
has_resume=0\n\
has_mcp_override=0\n\
for a in \"$@\"; do\n\
  case \"$a\" in\n\
    --output-schema) has_schema=1 ;;\n\
    resume) has_resume=1 ;;\n\
    mcp_servers={}) has_mcp_override=1 ;;\n\
  esac\n\
done\n\
test \"$has_mcp_override\" = 1 || exit 91\n";

    #[tokio::test]
    async fn a_phase_one_with_only_reads_triggers_one_nudge_that_edits_and_commits() {
        let dir = init_repo_with_commit();
        let base = crate::git::head(dir.path()).await.unwrap();
        let script = format!(
            "{NUDGE_FAKE_HEADER}{NUDGE_FAKE_PHASE_TWO}\
elif [ \"$has_resume\" = \"1\" ]; then\n\
  echo '{{\"type\":\"item.started\",\"item\":{{\"id\":\"n0\",\"type\":\"command_execution\",\"command\":\"write and commit\"}}}}'\n\
  echo 42 > answer.txt\n\
  git add -A\n\
  git commit -q -m nudge\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"n0\",\"type\":\"command_execution\",\"command\":\"write and commit\",\"exit_code\":0}}}}'\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"n1\",\"type\":\"agent_message\",\"text\":\"done\"}}}}'\n\
  echo '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}'\n\
else\n\
  echo '{{\"type\":\"thread.started\",\"thread_id\":\"nudge-sess-1\"}}'\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"i0\",\"type\":\"agent_message\",\"text\":\"I have analysed the code.\"}}}}'\n\
  echo '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}'\n\
fi\n"
        );
        let (out, log) =
            run_codex_with_fake(dir.path(), "nudge-test-noedit", &script, 3, &base).await;

        let nudges: Vec<&str> = log.lines().filter(|l| l.contains("forge_nudge")).collect();
        assert_eq!(
            nudges.len(),
            1,
            "one nudge makes the edit and commits it, so no more are needed: {log}"
        );
        assert!(
            nudges[0].contains("\"reason\":\"no-edit\""),
            "{}",
            nudges[0]
        );
        let head = crate::git::head(dir.path()).await.unwrap();
        assert_ne!(head, base, "the nudged turn committed");
        assert!(
            crate::git::dirty_paths(dir.path())
                .await
                .unwrap()
                .is_empty()
        );
        let structured: Value =
            serde_json::from_str(&out.structured.expect("phase two ran")).unwrap();
        assert_eq!(structured["summary"], "done");
    }

    #[tokio::test]
    async fn a_dirty_tree_triggers_the_commit_nudge() {
        let dir = init_repo_with_commit();
        let base = crate::git::head(dir.path()).await.unwrap();
        let script = format!(
            "{NUDGE_FAKE_HEADER}{NUDGE_FAKE_PHASE_TWO}\
elif [ \"$has_resume\" = \"1\" ]; then\n\
  echo '{{\"type\":\"item.started\",\"item\":{{\"id\":\"n0\",\"type\":\"command_execution\",\"command\":\"git commit\"}}}}'\n\
  git add -A\n\
  git commit -q -m nudge\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"n0\",\"type\":\"command_execution\",\"command\":\"git commit\",\"exit_code\":0}}}}'\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"n1\",\"type\":\"agent_message\",\"text\":\"done\"}}}}'\n\
  echo '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}'\n\
else\n\
  echo '{{\"type\":\"thread.started\",\"thread_id\":\"nudge-sess-2\"}}'\n\
  echo '{{\"type\":\"item.started\",\"item\":{{\"id\":\"i0\",\"type\":\"command_execution\",\"command\":\"echo 42 > answer.txt\"}}}}'\n\
  echo 42 > answer.txt\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"i0\",\"type\":\"command_execution\",\"command\":\"echo 42 > answer.txt\",\"exit_code\":0}}}}'\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"i1\",\"type\":\"agent_message\",\"text\":\"wrote the file\"}}}}'\n\
  echo '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}'\n\
fi\n"
        );
        let (out, log) =
            run_codex_with_fake(dir.path(), "nudge-test-dirty", &script, 3, &base).await;

        let nudges: Vec<&str> = log.lines().filter(|l| l.contains("forge_nudge")).collect();
        assert_eq!(
            nudges.len(),
            1,
            "the tree is clean and committed after one: {log}"
        );
        assert!(
            nudges[0].contains("\"reason\":\"uncommitted\""),
            "{}",
            nudges[0]
        );
        let head = crate::git::head(dir.path()).await.unwrap();
        assert_ne!(head, base, "the nudged turn committed the pending edit");
        assert!(
            crate::git::dirty_paths(dir.path())
                .await
                .unwrap()
                .is_empty()
        );
        let structured: Value =
            serde_json::from_str(&out.structured.expect("phase two ran")).unwrap();
        assert_eq!(structured["summary"], "done");
    }

    #[tokio::test]
    async fn nudges_zero_leaves_behavior_unchanged() {
        let dir = init_repo_with_commit();
        let base = crate::git::head(dir.path()).await.unwrap();
        // The same read-only phase one as the no-edit nudge test, but with
        // nudges = 0 no resume-without-schema call should ever be made; the
        // `elif` branch below is dead code, proof that reaching it would be
        // the bug.
        let script = format!(
            "{NUDGE_FAKE_HEADER}{NUDGE_FAKE_PHASE_TWO}\
elif [ \"$has_resume\" = \"1\" ]; then\n\
  echo 'should never run' >&2\n\
  exit 1\n\
else\n\
  echo '{{\"type\":\"thread.started\",\"thread_id\":\"nudge-sess-3\"}}'\n\
  echo '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"i0\",\"type\":\"agent_message\",\"text\":\"I have analysed the code.\"}}}}'\n\
  echo '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_output_tokens\":0}}}}'\n\
fi\n"
        );
        let (out, log) =
            run_codex_with_fake(dir.path(), "nudge-test-zero", &script, 0, &base).await;

        assert!(
            !log.contains("forge_nudge"),
            "nudges = 0 never resumes without a schema: {log}"
        );
        let head = crate::git::head(dir.path()).await.unwrap();
        assert_eq!(
            head, base,
            "phase one made no commit and nothing nudged it to"
        );
        let structured: Value =
            serde_json::from_str(&out.structured.expect("phase two still runs as today")).unwrap();
        assert_eq!(structured["summary"], "done");
    }
}
