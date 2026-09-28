//! Invocation and phase inputs shared by the agent runners.

use super::*;

/// A Codex phase with its command, environment, and streaming output sinks.
pub(super) struct RunCodexPhase<'a> {
    pub(super) l: &'a Launch<'a>,
    pub(super) argv: &'a [String],
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
    pub(super) extra_env: &'a [(String, String)],
    pub(super) start: &'a Instant,
    pub(super) log: &'a mut File,
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
}
