//! A job step's directive (docs/JOBS.md, "Steps"): a bounded model launch
//! whose structured output is held to the action's schema, or a fixture's
//! recorded output standing in for one.

use super::directive_text::{directive_inputs, directive_instructions, directive_prompt};
use crate::checks;
use crate::ctx::Forge;
use crate::workflows;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Job step inputs and role configuration for a model-backed directive.
pub(super) struct RunDirective<'a> {
    pub(super) f: &'a Forge,
    pub(super) job_id: i64,
    pub(super) seq: i64,
    pub(super) project_roles: &'a BTreeMap<String, String>,
    pub(super) step: &'a workflows::RunStep,
    pub(super) scratch: &'a Path,
    pub(super) idir: &'a Path,
    pub(super) input_text: &'a str,
    pub(super) step_outputs: &'a [(String, String)],
    pub(super) input_bytes: usize,
}

/// What a directive job step produced: the provider and model it ran under,
/// its cost, the check that judges it (a schema-valid structured output, or
/// the failure that means it never produced one), and — when the check
/// passed — its output as text, for later steps' inputs, and the file it
/// was written to.
pub(super) struct DirectiveOutcome {
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) cost_usd: f64,
    pub(super) check: checks::CheckResult,
    pub(super) output_text: String,
    pub(super) output_ref: Option<PathBuf>,
    pub(super) outcome: String,
    /// A jev judgment's probabilities, as JSON; empty for every other runner.
    pub(super) probabilities: String,
}

/// A job step's directive (docs/JOBS.md, "Steps"): a bounded launch with no
/// tools at all, its provider resolved from the step's `role` through the
/// existing `[roles]` layering, its structured output required against the
/// action's own `schema` before the next step can see it.
pub(super) async fn run_directive(args: RunDirective<'_>) -> Result<DirectiveOutcome> {
    let RunDirective {
        f,
        job_id,
        seq,
        project_roles,
        step,
        scratch,
        idir,
        input_text,
        step_outputs,
        input_bytes,
    } = args;
    let action = &step.action;
    // A directive job step's `role` is guaranteed non-empty by
    // `workflows::job_steps`, which resolved this step.
    let role = step.role.as_deref().unwrap_or_default();
    let provider = crate::ctx::resolve_provider(
        &f.providers,
        &f.roles,
        project_roles,
        &BTreeMap::new(),
        "",
        role,
    )?;
    let model = step
        .model
        .clone()
        .or_else(|| provider.model.clone())
        .unwrap_or_else(|| "sonnet".to_string());
    let max_turns = step.max_turns.unwrap_or(1);
    let timeout = Duration::from_secs(step.timeout_secs.unwrap_or(120) as u64);
    let system = directive_instructions(action);
    let prompt = directive_prompt(action, input_text, step_outputs, input_bytes);
    // Like an attempt's own log (`attempt::run_attempt`): the event stream
    // and stderr on disk under `FORGE_HOME/logs`, named so `forge job show`
    // can point a failed step's tail at it.
    let log_path = f.paths.logs.join(format!("job-{job_id}-{seq}.jsonl"));
    // Guaranteed present and valid JSON Schema by `workflows::job_steps`
    // and `parse_action`.
    let effective = action.effective_schema();
    let schema = effective.as_deref().unwrap_or("{}");

    let outcome = crate::directive::launch(
        f,
        crate::directive::Spec {
            id: job_id,
            step: action.name.as_str(),
            dir: scratch,
            prompt: &prompt,
            system: &system,
            model: &model,
            max_turns,
            timeout,
            check_timeout: std::time::Duration::ZERO,
            log_path: &log_path,
            provider,
            schema,
            sandboxed: false,
            writes: false,
            start_sha: "",
            resume: None,
            no_tools: true,
            judgment: Some(crate::agent::Judgment {
                action,
                state: &directive_inputs(input_text, step_outputs, input_bytes),
            }),
        },
    )
    .await?;

    let cost_usd = outcome.cost_usd.unwrap_or(0.0);
    let stderr_tail = checks::last_lines(&outcome.stderr_text, 20);

    // Whatever text the agent produced — its structured result, or the
    // plain text it returned instead when there was none — is written to
    // disk and named as the step's `output_ref`, pass or fail, the same as
    // an attempt leaves its own report behind: the point is never to have
    // to re-run a job just to see what the model actually said.
    let output_text = outcome
        .structured
        .clone()
        .or_else(|| (!outcome.result_text.is_empty()).then(|| outcome.result_text.clone()));
    let output_ref = output_text
        .as_ref()
        .map(|text| {
            let path = idir.join(format!("output-{}-{seq}.json", action.name));
            std::fs::write(&path, text)?;
            Ok::<_, anyhow::Error>(path)
        })
        .transpose()?;

    let fail = |tail: String| DirectiveOutcome {
        provider: provider.name.clone(),
        model: model.clone(),
        cost_usd,
        check: checks::CheckResult {
            level: "L0".to_string(),
            name: action.name.clone(),
            ok: false,
            tail,
            ..Default::default()
        },
        output_text: output_text.clone().unwrap_or_default(),
        output_ref: output_ref.clone(),
        outcome: String::new(),
        probabilities: String::new(),
    };
    if let Some(why) = crate::directive::failure(&outcome) {
        return Ok(fail(why.tail(&stderr_tail)));
    }
    let Some(structured) = &outcome.structured else {
        return Ok(fail("no structured output".to_string()));
    };
    let schema_value: serde_json::Value =
        serde_json::from_str(schema).context("the action's schema is not valid JSON")?;
    let instance: serde_json::Value = match serde_json::from_str(structured) {
        Ok(v) => v,
        Err(e) => {
            return Ok(fail(format!(
                "the structured output is not valid JSON: {e}"
            )));
        }
    };
    // A judgment is Jev's typed answer, not a model's attempt at the schema:
    // it is held to the action's outcomes instead.
    let judged = provider.runner == crate::agent::Runner::Jev;
    let invalid = if judged {
        crate::agent::check_judgment(action, &instance)
    } else {
        jsonschema::validate(&schema_value, &instance)
            .err()
            .map(|e| format!("the structured output does not match the schema: {e}"))
    };
    if let Some(why) = invalid {
        return Ok(fail(why));
    }

    Ok(DirectiveOutcome {
        provider: provider.name.clone(),
        model,
        cost_usd,
        check: checks::CheckResult {
            level: "L0".to_string(),
            name: action.name.clone(),
            ok: true,
            ..Default::default()
        },
        output_text: structured.clone(),
        output_ref,
        outcome: outcome_of(&instance),
        probabilities: crate::agent::probabilities(provider.runner, &instance),
    })
}

fn outcome_of(v: &serde_json::Value) -> String {
    v.get("outcome")
        .and_then(|o| o.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A directive step's stand-in during a fixture replay (docs/JOBS.md,
/// "Verifying an automation"): the structured output the fixture recorded
/// for it, in place of a model call, held to the action's own `schema`
/// exactly as a live output is, so a fixture cannot pin an output the step
/// would have refused. Cost nothing, launches nothing, needs no provider.
pub(super) fn recorded_directive(
    action: &workflows::ActionDef,
    recorded: &serde_json::Value,
    idir: &Path,
    seq: i64,
) -> Result<DirectiveOutcome> {
    let text = recorded.to_string();
    let path = idir.join(format!("output-{}-{seq}.json", action.name));
    std::fs::write(&path, &text)?;
    let effective = action.effective_schema();
    let schema: serde_json::Value = serde_json::from_str(effective.as_deref().unwrap_or("{}"))
        .context("the action's schema is not valid JSON")?;
    let (ok, tail) = match jsonschema::validate(&schema, recorded) {
        Ok(()) => (true, String::new()),
        Err(e) => (
            false,
            format!("the recorded output does not match the schema: {e}"),
        ),
    };
    Ok(DirectiveOutcome {
        provider: "recorded".to_string(),
        model: String::new(),
        cost_usd: 0.0,
        check: checks::CheckResult {
            level: "L0".to_string(),
            name: action.name.clone(),
            ok,
            tail,
            ..Default::default()
        },
        output_text: text,
        output_ref: Some(path),
        outcome: if ok {
            outcome_of(recorded)
        } else {
            String::new()
        },
        probabilities: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_action(description: &str, prompt: Option<&str>) -> workflows::ActionDef {
        workflows::ActionDef {
            name: "act".to_string(),
            kind: workflows::Kind::Directive,
            description: description.to_string(),
            consumes: vec![],
            produces: vec![],
            model: None,
            max_turns: None,
            timeout_secs: None,
            run: None,
            required_args: vec![],
            check: None,
            contract: workflows::Contract::Code,
            paths: vec![],
            brief: String::new(),
            prompt: prompt.map(str::to_string),
            prompt_file: None,
            prompt_hash: String::new(),
            includes: vec![],
            schema: None,
            outcomes: vec![],
            outcome_criteria: Default::default(),
            questions: vec![],
            confidence_below: vec![],
            file_into_initiative: false,
            overlay: false,
            verifies: false,
            output: workflows::Output::Tail,
            hash: String::new(),
            text: String::new(),
        }
    }

    #[test]
    fn directive_prompt_orders_input_then_each_step_output_under_the_cap() {
        let action = test_action("Do the thing.", Some("Extra instructions."));
        let prompt = directive_prompt(
            &action,
            "INPUT_DOC",
            &[
                ("step1".to_string(), "output1".to_string()),
                ("step2".to_string(), "output2".to_string()),
            ],
            10_000,
        );
        assert!(prompt.contains("This step: Do the thing."));
        assert!(prompt.contains("Extra instructions."));
        let input_pos = prompt.find("The input document:\nINPUT_DOC").unwrap();
        let step1_pos = prompt
            .find("The output of step \"step1\":\noutput1")
            .unwrap();
        let step2_pos = prompt
            .find("The output of step \"step2\":\noutput2")
            .unwrap();
        assert!(input_pos < step1_pos && step1_pos < step2_pos);
        assert!(!prompt.contains("cut to"));
    }

    #[test]
    fn directive_prompt_cuts_its_inputs_to_the_byte_cap() {
        let action = test_action("Do the thing.", None);
        let prompt = directive_prompt(&action, "INPUT_DOC", &[], 5);
        assert!(prompt.contains("cut to 5 bytes"));
    }

    #[test]
    fn a_recorded_output_is_held_to_the_actions_schema() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = test_action("judge", None);
        a.schema = Some(
            r#"{"type":"object","required":["kind"],"properties":{"kind":{"type":"string"}}}"#
                .to_string(),
        );
        let ok =
            recorded_directive(&a, &serde_json::json!({"kind": "bug"}), dir.path(), 0).unwrap();
        assert!(ok.check.ok);
        assert_eq!(ok.provider, "recorded");
        assert_eq!(ok.cost_usd, 0.0);
        assert_eq!(ok.output_text, r#"{"kind":"bug"}"#);
        let path = ok.output_ref.unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), r#"{"kind":"bug"}"#);
        let bad = recorded_directive(&a, &serde_json::json!({"kind": 3}), dir.path(), 0).unwrap();
        assert!(!bad.check.ok);
        assert!(
            bad.check.tail.contains("does not match the schema"),
            "{}",
            bad.check.tail
        );
    }
}
