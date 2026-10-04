//! Invocation and phase inputs shared by the agent runners.

use super::*;

/// A Codex phase with its command, environment, and streaming output sinks.
pub(super) struct RunCodexPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) prompt: &'a str,
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut CappedLog,
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
    pub(super) log: &'a mut CappedLog,
}

/// A JSON-streaming agent phase and the parser that folds frames into its outcome.
pub(super) struct RunJsonPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) prompt: &'a str,
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut CappedLog,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
    pub(super) apply:
        &'a mut (dyn FnMut(&Value, &mut Outcome, &mut Watch) -> Option<String> + Send + 'a),
}

/// A Copilot phase with its output sinks and cumulative token accounting.
pub(super) struct RunCopilotPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
    pub(super) prompt: &'a str,
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut CappedLog,
    pub(super) out: &'a mut Outcome,
    pub(super) watch: &'a mut Watch,
    pub(super) tally: &'a mut CopilotTally,
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
    fn provider_env_adds_the_runner_specific_key_only_when_the_named_variable_is_set() {
        // SAFETY: the variable name is unique to this test.
        unsafe { std::env::set_var("FORGE_TEST_PROVIDER_ENV_KEY", "secret-value") };
        let base = vec![("ALREADY".to_string(), "there".to_string())];
        for (runner, target) in [
            (Runner::ClaudeCli, "ANTHROPIC_API_KEY"),
            (Runner::CodexCli, "OPENAI_API_KEY"),
            (Runner::CopilotCli, "COPILOT_GITHUB_TOKEN"),
        ] {
            let provider = Provider {
                runner,
                api_key_env: Some("FORGE_TEST_PROVIDER_ENV_KEY".into()),
                env: base.clone(),
                ..Provider::default()
            };
            let env = provider_env(&provider);
            assert_eq!(
                env,
                vec![
                    ("ALREADY".to_string(), "there".to_string()),
                    (target.to_string(), "secret-value".to_string()),
                ]
            );
        }
        // SAFETY: cleanup of the same unique variable.
        unsafe { std::env::remove_var("FORGE_TEST_PROVIDER_ENV_KEY") };
    }

    #[test]
    fn provider_env_leaves_the_env_alone_for_chat_and_jev_and_without_an_api_key_env() {
        let base = vec![("ONLY".to_string(), "this".to_string())];
        let no_key_env = Provider {
            runner: Runner::ClaudeCli,
            api_key_env: None,
            env: base.clone(),
            ..Provider::default()
        };
        assert_eq!(provider_env(&no_key_env), base);

        // SAFETY: the variable name is unique to this test.
        unsafe { std::env::set_var("FORGE_TEST_PROVIDER_ENV_UNUSED", "x") };
        for runner in [Runner::Chat, Runner::Jev] {
            let provider = Provider {
                runner,
                api_key_env: Some("FORGE_TEST_PROVIDER_ENV_UNUSED".into()),
                env: base.clone(),
                ..Provider::default()
            };
            assert_eq!(provider_env(&provider), base);
        }
        // SAFETY: cleanup of the same unique variable.
        unsafe { std::env::remove_var("FORGE_TEST_PROVIDER_ENV_UNUSED") };
    }

    #[test]
    fn provider_env_skips_the_key_when_its_named_variable_is_unset() {
        // SAFETY: the variable name is unique to this test, and unset by it.
        unsafe { std::env::remove_var("FORGE_TEST_PROVIDER_ENV_MISSING") };
        let provider = Provider {
            runner: Runner::ClaudeCli,
            api_key_env: Some("FORGE_TEST_PROVIDER_ENV_MISSING".into()),
            ..Provider::default()
        };
        assert!(provider_env(&provider).is_empty());
    }

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
