//! The claude runner: the claude CLI's lean argv and one stream-json run
//! of it, relaunched on the bwrap race and priced from its own accounting.

use super::*;

/// The tools an attempt gets, and no other: what a coder, a reviewer or a
/// planner needs to read, search, edit and run. `StructuredOutput` is added
/// by `--json-schema` on top and is not a member of this list.
pub const ATTEMPT_TOOLS: &str = "Bash,Read,Edit,Write,Glob,Grep";

/// The claude CLI's argv for one launch: the flags common to every run,
/// `--json-schema` for the structured result every step (attempt or
/// directive) is held to, and `--tools`: exactly Bash, Read, Edit, Write,
/// Glob and Grep for an attempt, `""` when `no_tools` asks for a bounded
/// judgment with none.
///
/// The launch is lean, and unconditionally so: `--strict-mcp-config` (no
/// MCP server the operator configured), `--disable-slash-commands` (no
/// skills), `--setting-sources project,local` (the operator's user settings
/// stay out; the repository's own may apply) and
/// `--exclude-dynamic-system-prompt-sections`. Measured 2026-09-22 over 900
/// sandbox transcripts: an attempt's init event listed the operator's
/// claude.ai connectors (mail, drive, calendar, documents), 30+ skills, LSP
/// plugins and auto-memory, a security hole for an untrusted task and
/// about 16k tokens on every turn; the probe with these flags took turn-1
/// context from 33.5k to 17.2k and a second session wrote 0 (the prefix
/// reused across sessions). `--bare` was not usable: it refuses OAuth.
///
/// `--tools ""` rather than `--disallowedTools *` for the no-tools case —
/// they looked equivalent but are not: `--json-schema` forces a `StructuredOutput` tool into the run for
/// the model to answer through, and `*` denies that one too, so the model
/// can never submit its answer and the run ends at its turn cap with
/// `error_max_turns` (docs/JOBS.md, "Steps"; reproduced by hand against the
/// real CLI). `--tools ""` disables every other built-in tool while leaving
/// `StructuredOutput` (which is not itself a member of the built-in set)
/// reachable.
pub(super) fn claude_argv(bin: &str, l: &Launch<'_>) -> Vec<String> {
    let mut argv = vec![
        bin.to_string(),
        "--print".to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--strict-mcp-config".to_string(),
        "--disable-slash-commands".to_string(),
        "--setting-sources".to_string(),
        "project,local".to_string(),
        "--exclude-dynamic-system-prompt-sections".to_string(),
        "--model".to_string(),
        l.model.to_string(),
        "--max-turns".to_string(),
        l.max_turns.to_string(),
        "--json-schema".to_string(),
        l.schema.to_string(),
        "--tools".to_string(),
        if l.no_tools {
            String::new()
        } else {
            ATTEMPT_TOOLS.to_string()
        },
    ];
    argv.extend(l.provider.extra_args.iter().cloned());
    if let Some(id) = l.resume {
        argv.push("--resume".to_string());
        argv.push(id.to_string());
    }
    argv
}

/// Read the CLI result independently of the process exit status.
pub(super) fn apply_claude_result(out: &mut Outcome, v: &Value) {
    out.got_result = true;
    out.is_error = v["is_error"].as_bool().unwrap_or(false);
    if let Some(id) = v["session_id"].as_str() {
        out.session_id = Some(id.to_string());
    }
    out.subtype = v["subtype"].as_str().map(str::to_string);
    out.terminal_reason = v["terminal_reason"].as_str().map(str::to_string);
    out.max_turns_hit = v["subtype"].as_str() == Some("error_max_turns");
    refusal::read_claude_error(out, v);
    out.num_turns = v["num_turns"].as_i64().unwrap_or(0);
    out.cost_usd = v["total_cost_usd"].as_f64();
    out.input_tokens = v["usage"]["input_tokens"].as_i64();
    out.output_tokens = v["usage"]["output_tokens"].as_i64();
    out.cache_read_input_tokens = v["usage"]["cache_read_input_tokens"].as_i64();
    out.cache_creation_input_tokens = v["usage"]["cache_creation_input_tokens"].as_i64();
    out.result_text = v["result"].as_str().unwrap_or("").to_string();
    out.structured = match &v["structured_output"] {
        Value::Null => None,
        other => Some(other.to_string()),
    };
}

pub(super) async fn run_claude(l: Launch<'_>) -> Result<Outcome> {
    // The binary itself, never a version-manager shim: a shim inside the
    // sandbox reaches for state the sandbox does not have (a global tool
    // config, a registry cache, a writable shims directory) and dies
    // before the agent starts. Forge 1 learned this the same way.
    let bin = crate::executor::agent_bin(l.sandbox, l.worktree, agent_bin_for(l.step));
    let argv = claude_argv(&bin, &l);
    let mut identity = crate::git::identity(&l.worktree.join(".git")).await;
    identity.extend(inputs::provider_env(l.provider));
    let mut log = CappedLog::create(l.log_path)
        .with_context(|| format!("creating {}", l.log_path.display()))?;
    writeln!(
        log,
        "{{\"type\":\"forge_prompt\",\"text\":{}}}",
        serde_json::to_string(l.prompt)?
    )?;

    let (mut out, stderr_text) = run_with_relaunch(AgentRun {
        sandbox: l.sandbox,
        worktree: l.worktree,
        argv: &argv,
        identity: &identity,
        prompt: l.prompt,
        bin: &bin,
        timeout: l.timeout,
        writes: l.writes,
        early_ending: l.early_ending,
        task_id: l.task_id,
        report: l.report,
        log: &mut log,
    })
    .await?;

    if !stderr_text.trim().is_empty() {
        writeln!(
            log,
            "{{\"type\":\"forge_stderr\",\"text\":{}}}",
            serde_json::to_string(&stderr_text)?
        )?;
    }
    out.stderr_text = stderr_text;
    if let Some(prices) = crate::pricing::Prices::for_claude(l.provider) {
        crate::pricing::price_outcome(&mut out, &prices);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::tests::test_launch;
    use super::*;

    #[test]
    fn envelope_retry_exhaustion_is_classified_from_the_result_frame() {
        let frame = serde_json::json!({
            "type": "result",
            "is_error": true,
            "subtype": "error_max_structured_output_retries",
            "terminal_reason": "structured_output_retry_exhausted",
            "num_turns": 21
        });
        // Either diagnosis is sufficient across CLI versions.
        for omit in [None, Some("subtype"), Some("terminal_reason")] {
            let mut frame = frame.clone();
            if let Some(key) = omit {
                frame.as_object_mut().unwrap().remove(key);
            }
            let mut out = Outcome {
                exit_code: Some(1),
                ..Default::default()
            };
            apply_claude_result(&mut out, &frame);
            assert!(out.got_result && out.is_error);
            assert!(!out.max_turns_hit);
            assert_eq!(out.num_turns, 21);
            assert!(out.structured.is_none());
            assert_eq!(
                crate::directive::failure(&out),
                Some(crate::directive::Failure::StructuredOutput)
            );
            assert_eq!(
                crate::directive::agent_failure(&out).as_deref(),
                Some("structured_output_retry_exhausted: envelope missing")
            );
        }
    }

    /// `StructuredOutput` tool `--json-schema` itself forces into the run,
    /// so the model could never submit its answer and the run always ended
    /// at its turn cap (reproduced by hand against the real CLI: exit 1,
    /// `subtype: "error_max_turns"`). `--tools ""` disables the built-in set
    /// while leaving `StructuredOutput` reachable.
    #[test]
    fn claude_argv_with_no_tools_uses_the_tools_flag_not_disallowed_tools() {
        let dir = tempfile::tempdir().unwrap();
        let report = Reporter::new(false, None);
        let provider = Provider::default();
        let log_path = dir.path().join("log.jsonl");
        let l = test_launch(dir.path(), &report, &provider, &log_path, "{}", true, None);
        let argv = claude_argv("claude", &l);
        assert!(!argv.iter().any(|a| a == "--disallowedTools"), "{argv:?}");
        let tools_at = argv
            .iter()
            .position(|a| a == "--tools")
            .expect("--tools present: {argv:?}");
        assert_eq!(argv[tools_at + 1], "", "{argv:?}");
    }

    #[test]
    fn claude_argv_without_no_tools_names_exactly_the_attempt_tools() {
        let dir = tempfile::tempdir().unwrap();
        let report = Reporter::new(false, None);
        let provider = Provider::default();
        let log_path = dir.path().join("log.jsonl");
        let l = test_launch(dir.path(), &report, &provider, &log_path, "{}", false, None);
        let argv = claude_argv("claude", &l);
        let tools_at = argv.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(
            argv[tools_at + 1],
            "Bash,Read,Edit,Write,Glob,Grep",
            "{argv:?}"
        );
    }

    /// The lean flags are on every claude launch whatever the step: no MCP
    /// server, no skill, no user settings, no dynamic system-prompt
    /// sections. A security property, so it is asserted per step rather
    /// than trusted to the one builder.
    #[test]
    fn every_claude_launch_is_lean() {
        let dir = tempfile::tempdir().unwrap();
        let report = Reporter::new(false, None);
        let provider = Provider::default();
        let log_path = dir.path().join("log.jsonl");
        for (step, no_tools) in [
            ("code", false),
            ("review", false),
            ("investigate", false),
            ("supervisor", false),
            ("summarise", true),
        ] {
            let mut l = test_launch(
                dir.path(),
                &report,
                &provider,
                &log_path,
                "{}",
                no_tools,
                None,
            );
            l.step = step;
            let argv = claude_argv("claude", &l);
            for flag in [
                "--strict-mcp-config",
                "--disable-slash-commands",
                "--exclude-dynamic-system-prompt-sections",
            ] {
                assert!(
                    argv.iter().any(|a| a == flag),
                    "{step}: {flag} missing: {argv:?}"
                );
            }
            let at = argv.iter().position(|a| a == "--setting-sources").unwrap();
            assert_eq!(argv[at + 1], "project,local", "{step}: {argv:?}");
            assert!(
                !argv.iter().any(|a| a == "--bare"),
                "{step}: --bare refuses OAuth"
            );
            assert!(
                argv.iter().any(|a| a == "--json-schema"),
                "{step}: {argv:?}"
            );
        }
    }

    #[test]
    fn claude_argv_carries_the_schema_model_and_resume_id() {
        let dir = tempfile::tempdir().unwrap();
        let report = Reporter::new(false, None);
        let provider = Provider::default();
        let log_path = dir.path().join("log.jsonl");
        let schema = r#"{"type":"object"}"#;
        let l = test_launch(
            dir.path(),
            &report,
            &provider,
            &log_path,
            schema,
            true,
            Some("sess-1"),
        );
        let argv = claude_argv("claude", &l);
        assert_eq!(argv[0], "claude");
        let schema_at = argv.iter().position(|a| a == "--json-schema").unwrap();
        assert_eq!(argv[schema_at + 1], schema);
        let model_at = argv.iter().position(|a| a == "--model").unwrap();
        assert_eq!(argv[model_at + 1], "sonnet");
        let resume_at = argv.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(argv[resume_at + 1], "sess-1");
    }
}
