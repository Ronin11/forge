//! `Runner::Jev`: TypeSafe's Jev through Cloudflare Workers AI, typed
//! judgment in one HTTP call (docs/EXECUTION.md, "The judgment tier").

use super::{Launch, Outcome, truncated_first_line};
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::Write;
use std::time::Duration;
use std::time::Instant;

/// A job directive step's judgment for `Runner::Jev` (docs/EXECUTION.md,
/// "The judgment tier").
#[derive(Clone, Copy)]
pub struct Judgment<'a> {
    pub action: &'a crate::workflows::ActionDef,
    /// The step's prompt inputs: the input document and earlier outputs.
    pub state: &'a str,
}

/// Cloudflare Workers AI's run endpoint; `{account_id}` is the value of the
/// provider's `account_id_env`.
pub const JEV_DEFAULT_URL: &str =
    "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/run";
pub const JEV_DEFAULT_MODEL: &str = "typesafe/jev";
pub const JEV_DEFAULT_KEY_ENV: &str = "CLOUDFLARE_API_TOKEN";
pub const JEV_DEFAULT_ACCOUNT_ENV: &str = "CLOUDFLARE_ACCOUNT_ID";
/// List price of Jev's input tokens; its output is free.
pub const JEV_PRICE_INPUT_PER_MILLION: f64 = 0.042;

/// Jev's request for `action` over `state`: the outcomes as one `choice`
/// question named `outcome` whose criteria are their descriptions, then the
/// action's own `[[questions]]`.
pub fn request(model: &str, action: &crate::workflows::ActionDef, state: &str) -> Value {
    let mut instructions = action.description.clone();
    if let Some(extra) = &action.prompt {
        instructions.push('\n');
        instructions.push_str(extra);
    }
    let mut questions = serde_json::Map::new();
    questions.insert(
        crate::workflows::OUTCOME_QUESTION.into(),
        serde_json::json!({
            "type": "choice",
            "instructions": instructions,
            "criteria": action.outcome_criteria,
        }),
    );
    for q in &action.questions {
        let mut v = serde_json::json!({"type": q.kind, "instructions": q.instructions});
        if let Some(c) = &q.criteria {
            v["criteria"] = match c {
                // Jev takes a score's criteria as its level names in order.
                Value::Object(levels) if q.kind == "score" => {
                    levels.keys().cloned().collect::<Vec<_>>().into()
                }
                _ => c.clone(),
            };
        }
        questions.insert(q.name.clone(), v);
    }
    serde_json::json!({"model": model, "input": {"state": state, "questions": questions}})
}

/// One of Jev's typed answers read into a label, a confidence and the
/// probability of each label.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub label: String,
    pub confidence: f64,
    /// Label to probability, as a JSON object.
    pub probabilities: Value,
}

/// Reads one answer of any of Jev's three shapes: a `choice` names its
/// label; a `noul` is a bare `noul` in [0, 1] whose label is `true` at 0.5
/// or above, with confidence `|noul - 0.5| * 2`; a `score` is the most
/// probable level of its `probabilities`, named through its `legend`. A
/// missing `confidence` is the label's probability.
pub fn read_answer(a: &Value) -> Option<Answer> {
    if let Some(n) = a["noul"].as_f64() {
        return Some(Answer {
            label: (n >= 0.5).to_string(),
            confidence: (n - 0.5).abs() * 2.0,
            probabilities: serde_json::json!({"true": n, "false": 1.0 - n}),
        });
    }
    let legend = |k: &str| {
        a["legend"][k]
            .as_str()
            .map_or_else(|| k.to_string(), str::to_string)
    };
    let probabilities: serde_json::Map<String, Value> = a["probabilities"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (legend(k), v.clone()))
        .collect();
    let top = probabilities
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
        .max_by(|x, y| x.1.total_cmp(&y.1));
    let label = match &a["choice"] {
        Value::String(s) => s.clone(),
        _ => top.as_ref()?.0.clone(),
    };
    let confidence = a["confidence"]
        .as_f64()
        .or_else(|| probabilities.get(&label).and_then(Value::as_f64))
        .unwrap_or(0.0);
    Some(Answer {
        label,
        confidence,
        probabilities: Value::Object(probabilities),
    })
}

/// The outcome a judgment of this confidence routes on: the lowest
/// `confidence_below` floor it is under, else the choice itself.
pub fn confidence_floor(floors: &[(f64, String)], confidence: f64, choice: &str) -> String {
    floors
        .iter()
        .filter(|(t, _)| confidence < *t)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or_else(|| choice.to_string(), |(_, name)| name.clone())
}

/// The directive's structured envelope from Jev's `answers`: `outcome`
/// (floored), `confidence`, the outcome question's `probabilities`, and the
/// other questions' answers under `answers`.
fn envelope(action: &crate::workflows::ActionDef, answers: &Value) -> Result<Value> {
    let main = read_answer(&answers[crate::workflows::OUTCOME_QUESTION])
        .context("jev returned no answer for the outcomes question")?;
    let choice = main.label.as_str();
    let confidence = main.confidence;
    let outcome = confidence_floor(&action.confidence_below, confidence, choice);
    let mut env = serde_json::json!({
        "outcome": outcome,
        "confidence": confidence,
        "probabilities": main.probabilities,
    });
    if outcome != choice {
        env["choice"] = choice.into();
    }
    let others: serde_json::Map<String, Value> = answers
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.as_str() != crate::workflows::OUTCOME_QUESTION)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !others.is_empty() {
        env["answers"] = Value::Object(others);
    }
    Ok(env)
}

/// Why a judgment's envelope is unusable, if it is: its outcome must be one
/// of the action's.
pub fn check_judgment(action: &crate::workflows::ActionDef, envelope: &Value) -> Option<String> {
    let outcome = envelope["outcome"].as_str().unwrap_or_default();
    (!action.outcomes.iter().any(|o| o == outcome))
        .then(|| format!("the judgment's outcome {outcome:?} is not one of the action's outcomes"))
}

/// The probabilities of a judgment's envelope as JSON; empty for a step
/// no jev provider judged.
pub fn probabilities(runner: super::Runner, envelope: &Value) -> String {
    match runner {
        super::Runner::Jev => envelope["probabilities"].to_string(),
        _ => String::new(),
    }
}

/// The URL and bearer token a `jev` provider posts with, from the
/// environment variables it names.
pub fn endpoint(provider: &super::Provider) -> Result<(String, String)> {
    let name = &provider.name;
    let mut url = provider
        .base_url
        .clone()
        .unwrap_or_else(|| JEV_DEFAULT_URL.to_string());
    if url.contains("{account_id}") {
        let var = provider
            .account_id_env
            .as_deref()
            .unwrap_or(JEV_DEFAULT_ACCOUNT_ENV);
        let id = std::env::var(var).map_err(|_| {
            anyhow::anyhow!(
                "provider {name:?}: ${var} is not set (account_id_env names the environment variable that holds the Cloudflare account id)"
            )
        })?;
        url = url.replace("{account_id}", &id);
    }
    let key_var = provider
        .api_key_env
        .as_deref()
        .unwrap_or(JEV_DEFAULT_KEY_ENV);
    let key = std::env::var(key_var).map_err(|_| {
        anyhow::anyhow!(
            "provider {name:?}: ${key_var} is not set (api_key_env names the environment variable that holds the key, never the key itself)"
        )
    })?;
    Ok((url, key))
}

/// What a call's usage cost at the provider's prices, when it reported any.
pub fn usage_cost(
    provider: &super::Provider,
    input: Option<i64>,
    output: Option<i64>,
) -> Option<f64> {
    input.map(|i| {
        i as f64 * provider.price_input_per_million / 1_000_000.0
            + output.unwrap_or(0) as f64 * provider.price_output_per_million / 1_000_000.0
    })
}

/// One raw request through a `jev` provider: the `answers` object and the
/// usage cost, for a caller that builds its own questions (`forge eval jev`).
pub async fn ask(
    provider: &super::Provider,
    body: &Value,
    timeout: Duration,
) -> Result<(Value, f64)> {
    let (url, key) = endpoint(provider)?;
    let resp = post_json(&reqwest::Client::new(), &url, &key, body, timeout).await?;
    let result = if resp["result"]["result"].is_object() {
        &resp["result"]["result"]
    } else {
        &resp["result"]
    };
    let usage = &result["usage"];
    let cost = usage_cost(
        provider,
        usage["input_tokens"].as_i64(),
        usage["output_tokens"].as_i64(),
    );
    Ok((result["answers"].clone(), cost.unwrap_or(0.0)))
}

/// A job's directive step judged by Jev (docs/EXECUTION.md, "The judgment
/// tier"): one POST, no tools, typed answers back. `run` has already
/// refused anything but a directive whose action declares outcomes.
pub(super) async fn run(l: Launch<'_>) -> Result<Outcome> {
    let start = Instant::now();
    let judgment = match l.judgment {
        Some(j) if l.no_tools && !j.action.outcome_criteria.is_empty() => j,
        _ => anyhow::bail!(
            "the jev backend judges; it can only run a job's directive step whose action \
             declares outcomes (docs/EXECUTION.md, \"The judgment tier\"); route this \
             step's role to another provider instead"
        ),
    };
    let mut log =
        File::create(l.log_path).with_context(|| format!("creating {}", l.log_path.display()))?;
    let mut out = Outcome::default();
    let fail = |log: &mut File, out: &mut Outcome, why: String| -> Result<()> {
        out.exit_code = Some(1);
        out.stderr_text = why;
        out.wall_ms = start.elapsed().as_millis();
        writeln!(
            log,
            "{{\"type\":\"forge_stderr\",\"text\":{}}}",
            serde_json::to_string(&out.stderr_text)?
        )?;
        Ok(())
    };
    let (url, key) = match endpoint(l.provider) {
        Ok(found) => found,
        Err(e) => {
            fail(&mut log, &mut out, format!("{e:#}"))?;
            return Ok(out);
        }
    };
    let body = request(l.model, judgment.action, judgment.state);
    writeln!(log, "{{\"type\":\"forge_jev_request\",\"body\":{body}}}")?;
    let client = reqwest::Client::new();
    let resp = match post_json(&client, &url, &key, &body, l.timeout).await {
        Ok(v) => v,
        Err(e) => {
            fail(&mut log, &mut out, format!("{e:#}"))?;
            return Ok(out);
        }
    };
    writeln!(log, "{{\"type\":\"forge_jev_response\",\"body\":{resp}}}")?;
    let result = if resp["result"]["result"].is_object() {
        &resp["result"]["result"]
    } else {
        &resp["result"]
    };
    let usage = &result["usage"];
    out.input_tokens = usage["input_tokens"].as_i64();
    out.output_tokens = usage["output_tokens"].as_i64();
    out.cost_usd = usage_cost(l.provider, out.input_tokens, out.output_tokens);
    out.exit_code = Some(0);
    match envelope(judgment.action, &result["answers"]) {
        Ok(env) => {
            out.got_result = true;
            out.result_text = env.to_string();
            out.structured = Some(out.result_text.clone());
        }
        Err(e) => {
            out.exit_code = Some(1);
            out.stderr_text = format!("{e:#}");
        }
    }
    out.wall_ms = start.elapsed().as_millis();
    Ok(out)
}

/// One JSON POST with a bearer token, the response parsed as JSON on a 2xx.
async fn post_json(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    body: &Value,
    timeout: Duration,
) -> Result<Value> {
    let resp = client
        .post(url)
        .bearer_auth(key)
        .json(body)
        .timeout(timeout)
        .send()
        .await
        .context("sending the jev request")?;
    let status = resp.status();
    let text = resp.text().await.context("reading the jev response body")?;
    if !status.is_success() {
        anyhow::bail!(
            "jev endpoint returned {status}: {}",
            truncated_first_line(&text)
        );
    }
    serde_json::from_str(&text).context("parsing the jev response as JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::{ActionDef, Question};

    fn action() -> ActionDef {
        let text = "name = \"triage\"\nkind = \"directive\"\ncontract = \"plan\"\ndescription = \"Route the email.\"\nconfidence_below = { 0.6 = \"uncertain\" }\n[outcomes]\nreply = \"a person needs an answer\"\nignore = \"newsletters and noise\"\n";
        crate::workflows::parse_action(std::path::Path::new("triage.toml"), text, "h".into())
            .unwrap()
    }

    #[test]
    fn the_request_is_the_outcomes_as_one_choice_question_then_the_actions_own() {
        let mut a = action();
        a.questions.push(Question {
            name: "urgency".into(),
            kind: "score".into(),
            instructions: "How soon?".into(),
            criteria: Some(serde_json::json!(["low", "high"])),
        });
        let req = request("typesafe/jev", &a, "The input document:\nhi");
        assert_eq!(req["model"], "typesafe/jev");
        assert_eq!(req["input"]["state"], "The input document:\nhi");
        let q = &req["input"]["questions"];
        assert_eq!(q["outcome"]["type"], "choice");
        assert_eq!(q["outcome"]["instructions"], "Route the email.");
        assert_eq!(
            q["outcome"]["criteria"],
            serde_json::json!({"reply": "a person needs an answer", "ignore": "newsletters and noise"})
        );
        assert!(q["outcome"]["criteria"].get("uncertain").is_none());
        assert_eq!(q["urgency"]["type"], "score");
        assert_eq!(q["urgency"]["criteria"], serde_json::json!(["low", "high"]));
    }

    #[test]
    fn a_confidence_under_a_floor_takes_the_floors_outcome() {
        let floors = [(0.6, "uncertain".to_string()), (0.8, "check".to_string())];
        assert_eq!(confidence_floor(&floors, 0.5, "reply"), "uncertain");
        assert_eq!(confidence_floor(&floors, 0.7, "reply"), "check");
        assert_eq!(confidence_floor(&floors, 0.6, "reply"), "check");
        assert_eq!(confidence_floor(&floors, 0.95, "reply"), "reply");
        assert_eq!(confidence_floor(&[], 0.01, "reply"), "reply");
    }

    #[test]
    fn the_envelope_carries_outcome_confidence_and_probabilities() {
        let a = action();
        let answers = serde_json::json!({
            "outcome": {"type": "choice", "choice": "reply", "confidence": 0.4,
                        "probabilities": {"reply": 0.4, "ignore": 0.3}},
            "urgency": {"type": "score", "score": 3},
        });
        let env = envelope(&a, &answers).unwrap();
        assert_eq!(env["outcome"], "uncertain");
        assert_eq!(env["choice"], "reply");
        assert_eq!(env["confidence"], 0.4);
        assert_eq!(env["probabilities"]["ignore"], 0.3);
        assert_eq!(env["answers"]["urgency"]["score"], 3);
        assert!(envelope(&a, &serde_json::json!({})).is_err());
    }

    #[test]
    fn each_of_jevs_three_answer_shapes_is_read() {
        let choice = serde_json::json!({"type": "choice", "choice": "reply",
            "probabilities": {"reply": 0.9, "ignore": 0.1}, "confidence": 1});
        let a = read_answer(&choice).unwrap();
        assert_eq!(a.label, "reply");
        assert_eq!(a.confidence, 1.0);
        assert_eq!(a.probabilities["ignore"], 0.1);

        let a = read_answer(&serde_json::json!({"type": "noul", "noul": 0.22})).unwrap();
        assert_eq!(a.label, "false");
        assert!((a.confidence - 0.56).abs() < 1e-9, "{}", a.confidence);
        assert_eq!(a.probabilities["true"], 0.22);
        assert!((a.probabilities["false"].as_f64().unwrap() - 0.78).abs() < 1e-9);
        let a = read_answer(&serde_json::json!({"type": "noul", "noul": 0.5})).unwrap();
        assert_eq!((a.label.as_str(), a.confidence), ("true", 0.0));

        let score = serde_json::json!({"type": "score", "score": 1.21,
            "legend": {"0": "small", "1": "medium", "2": "large"},
            "probabilities": {"0": 0, "1": 0.78, "2": 0.22}, "confidence": 0.67});
        let a = read_answer(&score).unwrap();
        assert_eq!(a.label, "medium");
        assert_eq!(a.confidence, 0.67);
        assert_eq!(a.probabilities["large"], 0.22);

        assert!(read_answer(&serde_json::json!({})).is_none());
    }

    #[test]
    fn a_score_questions_criteria_go_as_its_level_names() {
        let mut a = action();
        a.questions.push(Question {
            name: "size".into(),
            kind: "score".into(),
            instructions: "How big?".into(),
            criteria: Some(serde_json::json!({"a-small": "few lines", "b-large": "many"})),
        });
        let req = request("typesafe/jev", &a, "s");
        assert_eq!(
            req["input"]["questions"]["size"]["criteria"],
            serde_json::json!(["a-small", "b-large"])
        );
    }
}
