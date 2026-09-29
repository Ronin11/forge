//! Invocation and phase inputs shared by the agent runners.

use super::*;

/// A Codex phase with its command, environment, and streaming output sinks.
pub(super) struct RunCodexPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) stdin: &'a str,
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut File,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
}

/// One agent invocation, including its sandbox, limits, identity, and output sinks.
pub(super) struct AgentRun<'a> {
    pub(super) sandbox: Option<&'a Execution>,
    pub(super) worktree: &'a Path,
    pub(super) argv: &'a [String],
    pub(super) identity: &'a [(String, String)],
    pub(super) prompt: &'a str,
    pub(super) bin: &'a str,
    pub(super) timeout: Duration,
    pub(super) writes: bool,
    pub(super) early_ending: crate::config::EarlyEnding,
    pub(super) task_id: i64,
    pub(super) report: &'a Reporter,
    pub(super) log: &'a mut File,
}

/// A JSON-streaming agent phase and the parser that folds frames into its outcome.
pub(super) struct RunJsonPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) stdin: &'a str,
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut File,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
    pub(super) apply:
        &'a mut (dyn FnMut(&Value, &mut Outcome, &mut Watch) -> Option<String> + Send + 'a),
}

/// A Copilot phase with its output sinks and cumulative token accounting.
pub(super) struct RunCopilotPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) stdin: &'a str,
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut File,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
    pub(super) tally: &'a mut CopilotTally,
}

/// The most prompt bytes the copilot CLI is handed: stdin could not be verified
/// non-interactively, so we retain `-p <text>`, one argv entry the kernel caps at 128 KiB
/// (`MAX_ARG_STRLEN`, past which `execve` fails E2BIG), so a longer one is cut
/// here, under that with room to spare.
pub(super) const COPILOT_PROMPT_LIMIT: usize = 96 * 1024;

/// `prompt` as copilot's `-p` argument: itself when it fits, else its head
/// (cut on a character boundary) and a note saying how much was dropped.
pub(super) fn cap_prompt(prompt: &str) -> String {
    if prompt.len() <= COPILOT_PROMPT_LIMIT {
        return prompt.to_string();
    }
    let note = format!(
        "\n\n[forge: this prompt was {} bytes; Forge passes it as a command-line \
         argument, so it was cut to {COPILOT_PROMPT_LIMIT} bytes and the rest is missing]",
        prompt.len()
    );
    let mut end = COPILOT_PROMPT_LIMIT - note.len();
    while !prompt.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{note}", &prompt[..end])
}

/// Writes `text` to the child's stdin and closes it, off to the side: a child
/// that exits or never reads must not block the caller on a full pipe, and a
/// write that lands on a closed pipe is nothing to fail the launch for.
pub(super) fn feed_stdin(child: &mut tokio::process::Child, text: &str) {
    if let Some(mut stdin) = child.stdin.take() {
        let text = text.to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(text.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
}

// Resolve portable API credentials at launch, never into attempt inputs.
pub(super) fn provider_env(provider: &Provider) -> Vec<(String, String)> {
    let mut env = provider.env.clone();
    if let Some(var) = &provider.api_key_env
        && let Ok(key) = std::env::var(var)
    {
        let target = match provider.runner {
            Runner::ClaudeCli => "ANTHROPIC_API_KEY",
            Runner::CodexCli => "OPENAI_API_KEY",
            Runner::CopilotCli => "COPILOT_GITHUB_TOKEN",
            Runner::Chat | Runner::Jev => return env,
        };
        env.push((target.into(), key));
    }
    env
}

/// Build Codex settings solely from the resolved Forge provider, never its
/// operator config. CLI arguments still select local providers and overrides.
pub(super) fn codex_config(provider: &Provider, model: &str) -> Result<String> {
    let mut config = toml::Table::new();
    if !model.is_empty() {
        config.insert("model".into(), model.into());
    }
    if let Some(url) = &provider.base_url {
        config.insert("model_provider".into(), provider.name.clone().into());
        let mut entry = toml::Table::new();
        entry.insert("name".into(), provider.name.clone().into());
        entry.insert("base_url".into(), url.clone().into());
        entry.insert("wire_api".into(), "responses".into());
        if provider.api_key_env.is_some() {
            entry.insert("env_key".into(), "OPENAI_API_KEY".into());
        }
        let mut providers = toml::Table::new();
        providers.insert(provider.name.clone(), entry.into());
        config.insert("model_providers".into(), providers.into());
    }
    Ok(toml::to_string(&config)?)
}

/// Write the strict codex schema to `path`, a file the sandbox can write, by
/// renaming a fresh sibling over it so a planted symlink is replaced, never
/// followed.
pub(super) fn write_codex_schema(path: &Path, schema: &str) -> Result<()> {
    let strict = strict_schema(schema)?;
    crate::login::replace_atomic(path, strict.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// The schema as OpenAI's strict structured output accepts it: every
/// object lists all of its properties as `required` and forbids
/// additional ones. Claude takes the schemas as written, with optional
/// keys; codex's phase two (`--output-schema`) is refused for the same
/// text ("'required' is required to be supplied and to be an array
/// including every key in properties", task 509, 2026-09-22). Nothing is
/// made nullable: an optional string or array becomes required and the
/// model sends it empty, which every envelope reader already treats as
/// absent, whereas an explicit `null` would fail the `#[serde(default)]`
/// fields.
pub fn strict_schema(schema: &str) -> Result<String> {
    let mut v: serde_json::Value =
        serde_json::from_str(schema).context("the output schema is not valid JSON")?;
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                let is_object = map.get("type").and_then(|t| t.as_str()) == Some("object")
                    || map.contains_key("properties");
                if is_object && let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    let keys: Vec<serde_json::Value> = props
                        .keys()
                        .map(|k| serde_json::Value::String(k.clone()))
                        .collect();
                    map.insert("required".into(), serde_json::Value::Array(keys));
                    map.insert(
                        "additionalProperties".into(),
                        serde_json::Value::Bool(false),
                    );
                }
                for (_, child) in map.iter_mut() {
                    walk(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items.iter_mut() {
                    walk(item);
                }
            }
            _ => {}
        }
    }
    walk(&mut v);
    Ok(serde_json::to_string(&v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_config_contains_only_the_resolved_model_and_provider() {
        let provider = Provider {
            name: "quoted.\"provider".into(),
            base_url: Some("https://model.example/v1".into()),
            api_key_env: Some("MODEL_TOKEN".into()),
            env: vec![("SECRET".into(), "operator-secret".into())],
            ..Provider::default()
        };
        let text = codex_config(&provider, "chosen-model").unwrap();
        assert!(!text.contains("operator-secret"));
        let config: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(config.len(), 3);
        assert_eq!(config["model"].as_str(), Some("chosen-model"));
        assert_eq!(
            config["model_provider"].as_str(),
            Some(provider.name.as_str())
        );
        let entry = &config["model_providers"][&provider.name];
        assert_eq!(entry["base_url"].as_str(), provider.base_url.as_deref());
        assert_eq!(entry["env_key"].as_str(), Some("OPENAI_API_KEY"));
        assert!(codex_config(&Provider::default(), "").unwrap().is_empty());
    }

    #[test]
    fn the_codex_schema_file_replaces_a_planted_symlink_and_never_writes_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "precious").unwrap();
        let path = dir.path().join("forge-1-schema.json");
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        write_codex_schema(&path, crate::envelope::SCHEMA).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_file()
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("schema_version")
        );
    }
    #[test]
    fn a_copilot_prompt_over_the_limit_is_cut_with_a_visible_note() {
        let short = "do the task";
        assert_eq!(cap_prompt(short), short);
        let long = "é".repeat(200 * 1024);
        let capped = cap_prompt(&long);
        assert!(capped.len() <= COPILOT_PROMPT_LIMIT);
        assert!(capped.len() < 100 * 1024);
        assert!(capped.contains("[forge: this prompt was 409600 bytes"));
        assert!(capped.starts_with("éé"));
    }

    /// A fake codex that fails when its argv is longer than 1 KiB and
    /// otherwise reports the bytes it read on stdin.
    #[tokio::test]
    async fn a_200_kib_codex_prompt_travels_on_stdin_not_argv() {
        use std::os::unix::fs::PermissionsExt;
        let repo = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let fake = scratch.path().join("codex-fake.sh");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             n=0; for a in \"$@\"; do n=$((n + ${#a} + 1)); done\n\
             if [ \"$n\" -gt 1024 ]; then echo \"argv is $n bytes\" >&2; exit 7; fi\n\
             got=$(wc -c)\n\
             echo '{\"type\":\"thread.started\",\"thread_id\":\"big-sess\"}'\n\
             echo \"{\\\"type\\\":\\\"forge_test_stdin\\\",\\\"bytes\\\":$got}\"\n\
             echo '{\"type\":\"item.completed\",\"item\":{\"id\":\"m\",\"type\":\"agent_message\",\"text\":\"ok\"}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        // SAFETY: the step name is unique to this test.
        unsafe { std::env::set_var("FORGE_CODEX_BIN_BIG_PROMPT", &fake) };
        let prompt = "x".repeat(200 * 1024);
        let log_path = scratch.path().join("log.jsonl");
        let report = crate::report::Reporter::new(false, None);
        let provider = Provider {
            runner: Runner::CodexCli,
            ..Provider::default()
        };
        let out = run_codex(Launch {
            identity: Vec::new(),
            task_id: 1,
            worktree: repo.path(),
            prompt: &prompt,
            system: "",
            model: "",
            max_turns: 30,
            timeout: Duration::from_secs(20),
            check_timeout: Duration::ZERO,
            log_path: &log_path,
            sandbox: None,
            report: &report,
            step: "big-prompt",
            provider: &provider,
            resume: None,
            writes: false,
            start_sha: "",
            schema: crate::envelope::SCHEMA,
            early_ending: crate::config::EarlyEnding {
                no_edit_calls: 100,
                edits_without_commit: 100,
                repeats: 100,
                signals_to_end: 0,
            },
            no_tools: false,
            judgment: None,
        })
        .await
        .unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            !out.stderr_text.contains("argv is"),
            "argv too long: {}",
            out.stderr_text
        );
        assert_eq!(out.exit_code, Some(0), "{log}");
        assert!(log.contains("\"bytes\":204800"), "{log}");
    }
}
