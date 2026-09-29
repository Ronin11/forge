//! The chat runner: a directive step answered by one chat completion
//! against an OpenAI-compatible endpoint, with no agent CLI at all.

use super::*;

/// The untrusted-data sentence every Forge prompt carries: `run_chat`'s own
/// system message when a caller passes none (`Launch::system` empty).
/// Every directive caller today (`job::run_directive`) passes its own,
/// already carrying this sentence, so the fallback is only ever exercised
/// by a future caller that forgets to.
const UNTRUSTED_DATA_SENTENCE: &str = crate::workflows::UNTRUSTED_DATA;

/// One request to an OpenAI-compatible `/chat/completions` endpoint, parsed
/// as JSON on a 2xx response; any other outcome (a non-2xx status, a
/// network failure, a body that is not JSON) is an error naming why, so the
/// caller can decide whether to fall back rather than fail outright.
async fn chat_once(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<Value> {
    let mut req = client.post(url).json(body).timeout(timeout);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.context("sending the chat request")?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .context("reading the chat response body")?;
    if !status.is_success() {
        anyhow::bail!(
            "chat endpoint returned {status}: {}",
            truncated_first_line(&text)
        );
    }
    serde_json::from_str(&text).context("parsing the chat response as JSON")
}

/// The first line of `text`, bounded to a sane length: a non-2xx response
/// body can be an HTML error page or a wall of JSON, neither of which
/// belongs whole in an error message.
pub(crate) fn truncated_first_line(text: &str) -> String {
    let first = text.lines().next().unwrap_or(text);
    first.chars().take(300).collect()
}

/// A job's directive step (docs/JOBS.md, "Steps") run with no agent CLI at
/// all: one chat completion against the provider's OpenAI-compatible
/// endpoint is the whole step. `run` refuses this runner for anything but
/// a directive (`l.no_tools`), so this never has to guard against a task's
/// code step landing here with nothing to act with.
///
/// Two requests, at most: the first asks the endpoint to hold the model to
/// `l.schema` itself (`response_format: json_schema`), which not every
/// OpenAI-compatible endpoint understands; a non-2xx response or a network
/// failure falls back to a second request with no `response_format`, the
/// schema quoted in the system message instead and the model told to
/// answer with only the JSON object. Either way, the response's content is
/// parsed and checked against `l.schema` before it is trusted as
/// `Outcome::structured` — the fallback path may be talking to a model
/// that ignores instructions, so nothing here takes its word for the shape
/// of its own answer.
pub(super) async fn run_chat(l: Launch<'_>) -> Result<Outcome> {
    let start = Instant::now();
    let mut log =
        File::create(l.log_path).with_context(|| format!("creating {}", l.log_path.display()))?;
    writeln!(
        log,
        "{{\"type\":\"forge_prompt\",\"text\":{}}}",
        serde_json::to_string(l.prompt)?
    )?;

    let mut out = Outcome::default();
    let base_url = match &l.provider.base_url {
        Some(u) => u,
        None => {
            out.exit_code = Some(1);
            out.stderr_text = format!(
                "provider {:?} needs a base_url for the chat runner",
                l.provider.name
            );
            out.wall_ms = start.elapsed().as_millis();
            writeln!(
                log,
                "{{\"type\":\"forge_stderr\",\"text\":{}}}",
                serde_json::to_string(&out.stderr_text)?
            )?;
            return Ok(out);
        }
    };
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let api_key = match &l.provider.api_key_env {
        Some(var) => match std::env::var(var) {
            Ok(k) => Some(k),
            Err(_) => {
                out.exit_code = Some(1);
                out.stderr_text = format!(
                    "provider {:?}: ${var} is not set (api_key_env names the environment \
                     variable that holds the key, never the key itself)",
                    l.provider.name
                );
                out.wall_ms = start.elapsed().as_millis();
                writeln!(
                    log,
                    "{{\"type\":\"forge_stderr\",\"text\":{}}}",
                    serde_json::to_string(&out.stderr_text)?
                )?;
                return Ok(out);
            }
        },
        None => None,
    };

    let system = if l.system.is_empty() {
        UNTRUSTED_DATA_SENTENCE
    } else {
        l.system
    };
    let schema_value: Value =
        serde_json::from_str(l.schema).unwrap_or(Value::Object(Default::default()));

    let client = reqwest::Client::new();
    let schema_body = serde_json::json!({
        "model": l.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": l.prompt},
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "step_output", "schema": schema_value, "strict": true},
        },
    });

    let resp_json = match chat_once(&client, &url, api_key.as_deref(), &schema_body, l.timeout)
        .await
    {
        Ok(v) => v,
        Err(schema_err) => {
            writeln!(
                log,
                "{{\"type\":\"forge_chat_fallback\",\"reason\":{}}}",
                serde_json::to_string(&format!("{schema_err:#}"))?
            )?;
            let fallback_system = format!(
                "{system}\n\nRespond with only the JSON object described by this JSON Schema; \
                 no other text, no markdown fence:\n{}",
                l.schema
            );
            let fallback_body = serde_json::json!({
                "model": l.model,
                "messages": [
                    {"role": "system", "content": fallback_system},
                    {"role": "user", "content": l.prompt},
                ],
            });
            match chat_once(&client, &url, api_key.as_deref(), &fallback_body, l.timeout).await {
                Ok(v) => v,
                Err(fallback_err) => {
                    out.exit_code = Some(1);
                    out.stderr_text = format!(
                        "schema request: {schema_err:#}\nfallback request: {fallback_err:#}"
                    );
                    out.wall_ms = start.elapsed().as_millis();
                    writeln!(
                        log,
                        "{{\"type\":\"forge_stderr\",\"text\":{}}}",
                        serde_json::to_string(&out.stderr_text)?
                    )?;
                    return Ok(out);
                }
            }
        }
    };
    writeln!(
        log,
        "{{\"type\":\"forge_chat_response\",\"body\":{resp_json}}}"
    )?;

    out.exit_code = Some(0);
    out.got_result = true;
    let usage = &resp_json["usage"];
    out.input_tokens = usage["prompt_tokens"].as_i64();
    out.output_tokens = usage["completion_tokens"].as_i64();
    if let (Some(i), Some(o)) = (out.input_tokens, out.output_tokens) {
        out.cost_usd = Some(
            i as f64 * l.provider.price_input_per_million / 1_000_000.0
                + o as f64 * l.provider.price_output_per_million / 1_000_000.0,
        );
    }
    let content = resp_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    out.result_text = content.clone();
    if let Ok(instance) = serde_json::from_str::<Value>(&content)
        && jsonschema::validate(&schema_value, &instance).is_ok()
    {
        out.structured = Some(content);
    }
    out.wall_ms = start.elapsed().as_millis();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::tests::thresholds;
    use super::*;
    use std::path::PathBuf;

    /// A minimal JSON Schema `run_chat` validates the model's answer
    /// against: one required string field, enough to tell a real answer
    /// from garbage without dragging in `envelope::SCHEMA`'s full shape.
    const CHAT_TEST_SCHEMA: &str =
        r#"{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]}"#;

    /// A fake `/chat/completions` endpoint on loopback: `responses` is one
    /// `(status, body)` pair per request it will answer, in order: the
    /// schema-mode attempt first, then the fallback if there is a second.
    /// Returns the base URL to give `Provider::base_url` and a channel
    /// carrying each request's parsed JSON body, in the order received, so
    /// a test can assert on what `run_chat` actually sent.
    fn fake_chat_server(
        responses: Vec<(u16, String)>,
    ) -> (String, std::sync::mpsc::Receiver<Value>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server
            .server_addr()
            .to_ip()
            .expect("a loopback TCP listener always has an IP address");
        let url = format!("http://{addr}");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let mut req = server.recv().expect("the test sent a request");
                let mut content = String::new();
                std::io::Read::read_to_string(req.as_reader(), &mut content).unwrap();
                let parsed: Value = serde_json::from_str(&content).unwrap();
                tx.send(parsed).unwrap();
                let response = tiny_http::Response::from_string(body)
                    .with_status_code(tiny_http::StatusCode(status))
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    );
                req.respond(response).unwrap();
            }
        });
        (url, rx)
    }

    fn chat_provider(base_url: &str) -> Provider {
        Provider {
            runner: Runner::Chat,
            base_url: Some(base_url.to_string()),
            price_input_per_million: 1.0,
            price_output_per_million: 2.0,
            ..Provider::default()
        }
    }

    // Reason: test fixture helper keeps independently varied inputs explicit.
    #[allow(clippy::too_many_arguments)]
    fn chat_launch<'a>(
        worktree: &'a Path,
        report: &'a Reporter,
        provider: &'a Provider,
        log_path: &'a PathBuf,
        schema: &'a str,
        system: &'a str,
        prompt: &'a str,
    ) -> Launch<'a> {
        Launch {
            task_id: 1,
            worktree,
            identity: Vec::new(),
            prompt,
            system,
            model: "test-model",
            max_turns: 1,
            timeout: Duration::from_secs(5),
            check_timeout: Duration::ZERO,
            log_path,
            sandbox: None,
            report,
            step: "extract",
            provider,
            resume: None,
            start_sha: "",
            writes: false,
            schema,
            early_ending: thresholds(100, 100, 100, 2),
            no_tools: true,
            judgment: None,
        }
    }

    fn completion_body(content: &str, prompt_tokens: i64, completion_tokens: i64) -> String {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": content}}],
            "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens},
        })
        .to_string()
    }

    #[tokio::test]
    async fn chat_runner_uses_response_format_json_schema_and_records_tokens_and_cost() {
        let dir = tempfile::tempdir().unwrap();
        let (base_url, rx) = fake_chat_server(vec![(
            200,
            completion_body(r#"{"summary":"a bug in the parser"}"#, 100, 20),
        )]);
        let provider = chat_provider(&base_url);
        let report = Reporter::new(false, None);
        let log_path = dir.path().join("log.jsonl");
        let l = chat_launch(
            dir.path(),
            &report,
            &provider,
            &log_path,
            CHAT_TEST_SCHEMA,
            "You are triaging a bug report.",
            "The input document:\ntitle: parser crashes on empty input",
        );

        let out = run(l).await.unwrap();

        assert_eq!(
            out.structured.as_deref(),
            Some(r#"{"summary":"a bug in the parser"}"#)
        );
        assert_eq!(out.input_tokens, Some(100));
        assert_eq!(out.output_tokens, Some(20));
        // 100 * 1.0/1e6 + 20 * 2.0/1e6.
        assert!((out.cost_usd.unwrap() - 0.0001_40).abs() < 1e-9);
        assert_eq!(out.exit_code, Some(0));
        assert!(out.got_result);
        assert!(!out.is_error);

        let body = rx.recv().unwrap();
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][0]["content"],
            "You are triaging a bug report."
        );
        assert_eq!(body["messages"][1]["role"], "user");
        assert!(
            body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("parser crashes on empty input")
        );
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            body["response_format"]["json_schema"]["schema"],
            serde_json::from_str::<Value>(CHAT_TEST_SCHEMA).unwrap()
        );

        assert!(
            rx.try_recv().is_err(),
            "only one request: the endpoint accepted response_format"
        );
    }

    #[tokio::test]
    async fn chat_runner_falls_back_to_a_plain_instruction_when_response_format_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (base_url, rx) = fake_chat_server(vec![
            (
                400,
                r#"{"error":"unsupported parameter: response_format"}"#.to_string(),
            ),
            (
                200,
                completion_body(r#"{"summary":"fallback answer"}"#, 40, 8),
            ),
        ]);
        let provider = chat_provider(&base_url);
        let report = Reporter::new(false, None);
        let log_path = dir.path().join("log.jsonl");
        let l = chat_launch(
            dir.path(),
            &report,
            &provider,
            &log_path,
            CHAT_TEST_SCHEMA,
            "You are triaging a bug report.",
            "The input document:\ntitle: crash",
        );

        let out = run(l).await.unwrap();

        assert_eq!(
            out.structured.as_deref(),
            Some(r#"{"summary":"fallback answer"}"#)
        );
        assert_eq!(out.input_tokens, Some(40));
        assert_eq!(out.output_tokens, Some(8));

        let first = rx.recv().unwrap();
        assert_eq!(first["response_format"]["type"], "json_schema");
        let second = rx.recv().unwrap();
        assert!(
            second.get("response_format").is_none(),
            "the fallback drops response_format entirely: {second}"
        );
        assert!(
            second["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("only the JSON object"),
            "{second}"
        );
    }

    #[tokio::test]
    async fn chat_runner_leaves_structured_none_when_the_fallback_answer_does_not_fit_the_schema() {
        let dir = tempfile::tempdir().unwrap();
        let (base_url, _rx) = fake_chat_server(vec![
            (400, "{}".to_string()),
            // No "summary" field: fails CHAT_TEST_SCHEMA's own `required`.
            (200, completion_body(r#"{"wrong_field":"nope"}"#, 10, 5)),
        ]);
        let provider = chat_provider(&base_url);
        let report = Reporter::new(false, None);
        let log_path = dir.path().join("log.jsonl");
        let l = chat_launch(
            dir.path(),
            &report,
            &provider,
            &log_path,
            CHAT_TEST_SCHEMA,
            "system",
            "prompt",
        );

        let out = run(l).await.unwrap();
        assert_eq!(
            out.structured, None,
            "invalid against the schema, never trusted"
        );
        assert!(out.got_result, "the endpoint did answer, just not validly");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn chat_runner_is_refused_for_a_step_that_is_not_a_directive() {
        let dir = tempfile::tempdir().unwrap();
        let (base_url, _rx) = fake_chat_server(vec![]);
        let provider = chat_provider(&base_url);
        let report = Reporter::new(false, None);
        let log_path = dir.path().join("log.jsonl");
        let mut l = chat_launch(
            dir.path(),
            &report,
            &provider,
            &log_path,
            CHAT_TEST_SCHEMA,
            "system",
            "prompt",
        );
        l.no_tools = false;

        let err = run(l).await.unwrap_err();
        assert!(err.to_string().contains("no tools"), "{err}");
    }
}
