//! docs/EXECUTION.md, "The judgment tier": a directive step judged by a
//! `jev` provider against a fake endpoint answering TypeSafe's documented
//! shape at `/v1/systemone` and Cloudflare Workers AI's under `/accounts/`.

use crate::support::*;
use std::sync::mpsc::Receiver;

const ACTION: &str = r#"name = "triage"
kind = "directive"
contract = "plan"
description = "sort a message"
confidence_below = { 0.6 = "uncertain" }
schema = '''
{"type":"object"}
'''

[outcomes]
reply = "a person needs an answer"
ignore = "noise"
"#;

const WORKFLOW: &str = r#"name = "triage-flow"
kind = "run"
description = "routes on the judged outcome"

steps = [
  { action = "triage", role = "plan", judgment = "an unknown sender's ask is not a pattern a rule can match", on = { uncertain = "ask-april" } },
  { action = "ask-april" },
]

[trigger]
on = "manual"

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#;

const ASK: &str = "name = \"ask-april\"\nkind = \"operation\"\ndescription = \"hand it to a person\"\nrun = [\"true\"]\n";

/// Jev's answer to one question of the request, in the shape the real API
/// returns for its type: a `choice` of `choice` at `confidence`, a `noul`
/// of 0.22, a `score` whose most probable level is the second; `Err` is the
/// API's refusal of a score whose criteria are not an array of level names.
fn answer(
    name: &str,
    q: &serde_json::Value,
    choice: &str,
    confidence: f64,
) -> Result<serde_json::Value, String> {
    match q["type"].as_str() {
        Some("choice") => {
            let options = q["criteria"].as_object().cloned().unwrap_or_default();
            let rest = (1.0 - confidence) / (options.len().max(2) - 1) as f64;
            let probabilities: serde_json::Map<String, serde_json::Value> = options
                .keys()
                .map(|k| {
                    (
                        k.clone(),
                        if k == choice { confidence } else { rest }.into(),
                    )
                })
                .collect();
            Ok(serde_json::json!({"type": "choice", "choice": choice,
                "probabilities": probabilities, "confidence": confidence}))
        }
        Some("noul") => Ok(serde_json::json!({"type": "noul", "noul": 0.22})),
        Some("score") => {
            let Some(levels) = q["criteria"].as_array() else {
                return Err(format!(
                    "Invalid value at questions.{name}.criteria: Invalid input: expected array, received object"
                ));
            };
            let legend: serde_json::Map<String, serde_json::Value> = levels
                .iter()
                .enumerate()
                .map(|(i, l)| (i.to_string(), l.clone()))
                .collect();
            let probabilities: serde_json::Map<String, serde_json::Value> = (0..levels.len())
                .map(|i| {
                    let p = match i {
                        1 => 0.78,
                        2 => 0.22,
                        _ => 0.0,
                    };
                    (i.to_string(), p.into())
                })
                .collect();
            Ok(
                serde_json::json!({"type": "score", "score": 1.21, "legend": legend,
                "probabilities": probabilities, "confidence": 0.67}),
            )
        }
        other => Err(format!("unknown question type {other:?}")),
    }
}

/// How the fake answers besides a judgment: Cloudflare's 402 (its gateway
/// out of credits), or TypeSafe's 529 (overloaded) for its first `busy`
/// requests.
#[derive(Clone, Copy, Default)]
struct Fake {
    cloudflare_broke: bool,
    busy: usize,
}

/// A loopback endpoint answering each request as TypeSafe (any path but
/// `/accounts/...`) or Cloudflare Workers AI answers Jev's: each question
/// in the shape its type takes (see `answer`), a choice being `choice` at
/// `confidence`; the receiver carries (path, authorization, body) per
/// request.
fn fake_jev(
    choice: &'static str,
    confidence: f64,
) -> (String, Receiver<(String, String, serde_json::Value)>) {
    fake_jev_with(choice, confidence, Fake::default())
}

fn fake_jev_with(
    choice: &'static str,
    confidence: f64,
    fake: Fake,
) -> (String, Receiver<(String, String, serde_json::Value)>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut busy = fake.busy;
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            std::io::Read::read_to_string(req.as_reader(), &mut body).unwrap();
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            let path = req.url().to_string();
            let _ = tx.send((path.clone(), auth, body.clone()));
            let cloudflare = path.starts_with("/accounts/");
            if cloudflare && fake.cloudflare_broke {
                let _ = req.respond(
                    tiny_http::Response::from_string(
                        r#"{"success":false,"errors":[{"code":2003,"message":"Insufficient balance"}]}"#,
                    )
                    .with_status_code(402),
                );
                continue;
            }
            if !cloudflare && busy > 0 {
                busy -= 1;
                let _ = req.respond(
                    tiny_http::Response::from_string(r#"{"error":"overloaded"}"#)
                        .with_status_code(529),
                );
                continue;
            }
            let questions = if cloudflare {
                &body["input"]["questions"]
            } else {
                &body["questions"]
            };
            let answers: Result<serde_json::Map<String, serde_json::Value>, String> = questions
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, q)| Ok((name.clone(), answer(name, q, choice, confidence)?)))
                .collect();
            let result = |answers| {
                serde_json::json!({
                    "model": "jev-1.13.0",
                    "answers": answers,
                    "usage": {"input_tokens": 500, "output_tokens": 87},
                })
            };
            let response = match answers {
                Ok(answers) if cloudflare => tiny_http::Response::from_string(
                    serde_json::json!({"success": true, "result": {
                        "state": "Completed",
                        "result": result(answers),
                    }})
                    .to_string(),
                ),
                Ok(answers) => tiny_http::Response::from_string(result(answers).to_string()),
                Err(why) => tiny_http::Response::from_string(
                    serde_json::json!({"error": {"message": why}}).to_string(),
                )
                .with_status_code(422),
            };
            let _ = req.respond(response);
        }
    });
    (url, rx)
}

/// A jev provider with both hosts on the fake, and the triage workflow.
fn setup(e: &Env, url: &str, extra: &str) {
    let repo_s = e.repo.to_str().unwrap();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        format!(
            "[providers.jev]\nrunner = \"jev\"\nbase_url = \"{url}/v1/systemone\"\ncloudflare_url = \"{url}/accounts/{{account_id}}/ai/run\"\n{extra}[roles]\nplan = \"jev\"\n"
        ),
    )
    .unwrap();
    let p = [
        "project",
        "new",
        "equitizr",
        "--purpose",
        "p",
        "--repo",
        repo_s,
    ];
    assert!(e.forge("ok.sh", &p).status.success());
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let d = e.home.join("workflows");
    std::fs::write(d.join("actions/triage.toml"), ACTION).unwrap();
    std::fs::write(d.join("actions/ask-april.toml"), ASK).unwrap();
    std::fs::write(d.join("triage-flow.toml"), WORKFLOW).unwrap();
}

/// One `forge job start --now` of the triage workflow with `env` set:
/// the job's `show --json`, and the run's stderr.
fn start(e: &Env, env: &[(&str, &str)]) -> (serde_json::Value, String) {
    let mut c = e.cmd("ok.sh");
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c
        .env("FORGE_JEV_BACKOFF_MS", "10")
        .args(["job", "start", "equitizr", "triage-flow", "--now"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    let id: i64 = out.trim().parse().unwrap_or_else(|_| panic!("{out}{err}"));
    let shown = e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"]);
    (serde_json::from_slice(&shown.stdout).unwrap(), err)
}

const TYPESAFE: &[(&str, &str)] = &[("TYPESAFE_API_KEY", "ts-1")];
const BOTH: &[(&str, &str)] = &[
    ("TYPESAFE_API_KEY", "ts-1"),
    ("CLOUDFLARE_ACCOUNT_ID", "acct-1"),
    ("CLOUDFLARE_API_TOKEN", "tok-1"),
];

fn judged_run(e: &Env, url: &str) -> serde_json::Value {
    setup(e, url, "");
    start(e, TYPESAFE).0
}

fn marker(e: &Env) -> std::path::PathBuf {
    e.xdg_config
        .join("systemd/user/forge-worker.service.d/jev-cloudflare-exhausted")
}

#[test]
fn a_jev_step_routes_on_its_outcome_and_a_low_confidence_is_its_own_outcome() {
    let e = Env::new();
    let (url, rx) = fake_jev("reply", 0.4);
    let doc = judged_run(&e, &url);
    assert_eq!(doc["state"], "ok", "{doc}");
    let step = &doc["steps"][0];
    assert_eq!(step["outcome"], "uncertain", "{doc}");
    assert_eq!(step["provider"], "jev", "{doc}");
    assert_eq!(step["probabilities"]["reply"], 0.4, "{doc}");
    assert_eq!(doc["steps"][1]["node"], "1-ask-april", "{doc}");
    // 500 input tokens at $0.042 per million; output is free.
    let cost = step["cost_usd"].as_f64().unwrap();
    assert!((cost - 500.0 * 0.042 / 1e6).abs() < 1e-12, "{cost}");

    let (path, auth, body) = rx.recv().unwrap();
    assert_eq!(path, "/v1/systemone");
    assert_eq!(auth, "Bearer ts-1");
    assert_eq!(body["model"], "jev-latest");
    assert!(body.get("input").is_none(), "{body}");
    assert_eq!(
        body["questions"]["outcome"]["criteria"]["reply"],
        "a person needs an answer"
    );
    assert!(
        body["state"]
            .as_str()
            .unwrap()
            .contains("The input document")
    );
}

#[test]
fn a_confident_judgment_keeps_its_choice() {
    let e = Env::new();
    let (url, _rx) = fake_jev("ignore", 0.9);
    let doc = judged_run(&e, &url);
    assert_eq!(doc["state"], "ok", "{doc}");
    assert_eq!(doc["steps"][0]["outcome"], "ignore", "{doc}");
}

#[test]
fn eval_jev_replays_a_fixture_and_reports_accuracy_calibration_latency_and_cost() {
    let e = Env::new();
    let (url, rx) = fake_jev("request", 0.9);
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        format!("[providers.jev]\nrunner = \"jev\"\nbase_url = \"{url}/v1/systemone\"\n"),
    )
    .unwrap();
    let fixture = e.home.join("fixture.json");
    std::fs::write(
        &fixture,
        r#"{"concierge": [
              {"state": "make the quote say usually same day", "label": "request"},
              {"state": "did the reminder go out?", "label": "question"}],
            "demotions": [
              {"state": "cargo test parse::edge fails with left 3, right 4", "label": "yes"},
              {"state": "should this also cover the legacy path?", "label": "no"}],
            "size": [
              {"state": "fix the typo in the quote footer", "label": "small"},
              {"state": "add a reminders page with its own settings", "label": "medium"}]}"#,
    )
    .unwrap();
    let report = e.home.join("report.md");
    let o = e
        .cmd("ok.sh")
        .env("TYPESAFE_API_KEY", "ts-1")
        .args(["eval", "jev", "--provider", "jev", "--fixture"])
        .arg(&fixture)
        .arg("--out")
        .arg(&report)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&report).unwrap().trim_end(),
        out.trim_end()
    );
    let section = |title: &str| {
        let at = out
            .find(&format!("## {title}"))
            .unwrap_or_else(|| panic!("no {title} in {out}"));
        let rest = &out[at + 3..];
        rest[..rest.find("## ").unwrap_or(rest.len())].to_string()
    };
    let concierge = section("Concierge decisions");
    assert!(
        concierge.contains("accuracy: 50.0% (1 of 2 answered)"),
        "{out}"
    );
    assert!(
        concierge.contains("| 0.9-1.0 | 2 | 0.90 | 50.0% |"),
        "{out}"
    );
    assert!(concierge.contains("mean latency:"), "{out}");
    // Two calls of 500 input tokens at $0.042 per million.
    assert!(concierge.contains("total cost: $0.000042"), "{out}");
    // A noul of 0.22 is `no` at confidence 0.56.
    let demotions = section("Review demotions");
    assert!(
        demotions.contains("accuracy: 50.0% (1 of 2 answered)"),
        "{out}"
    );
    assert!(
        demotions.contains("| 0.5-0.6 | 2 | 0.56 | 50.0% |"),
        "{out}"
    );
    // The most probable level is `medium`, at the answer's confidence.
    let size = section("Task size");
    assert!(size.contains("accuracy: 50.0% (1 of 2 answered)"), "{out}");
    assert!(size.contains("| 0.6-0.7 | 2 | 0.67 | 50.0% |"), "{out}");
    for s in [&concierge, &demotions, &size] {
        assert!(s.contains("- errors: 0\n"), "{out}");
    }
    let bodies: Vec<serde_json::Value> = rx.try_iter().map(|(_, _, b)| b).collect();
    assert_eq!(bodies.len(), 6);
    let q = &bodies[0]["questions"]["outcome"];
    assert_eq!(q["type"], "choice");
    assert!(q["criteria"]["need"].is_string());
    let q = &bodies[2]["questions"]["outcome"];
    assert_eq!(q["type"], "noul");
    assert!(q["criteria"]["true"].is_string() && q["criteria"]["false"].is_string());
    let q = &bodies[4]["questions"]["outcome"];
    assert_eq!(q["type"], "score");
    assert_eq!(
        q["criteria"],
        serde_json::json!(["small", "medium", "large"])
    );
}

#[test]
fn cloudflare_goes_first_while_its_credits_last() {
    let e = Env::new();
    let (url, rx) = fake_jev("ignore", 0.9);
    setup(&e, &url, "");
    let (doc, _) = start(&e, BOTH);
    assert_eq!(doc["state"], "ok", "{doc}");
    assert_eq!(doc["steps"][0]["outcome"], "ignore", "{doc}");
    let (path, auth, body) = rx.recv().unwrap();
    assert_eq!(path, "/accounts/acct-1/ai/run");
    assert_eq!(auth, "Bearer tok-1");
    assert_eq!(body["model"], "typesafe/jev");
    assert!(body["input"]["questions"]["outcome"].is_object(), "{body}");
    assert!(rx.try_recv().is_err());
    assert!(!marker(&e).exists());
}

#[test]
fn cloudflares_402_switches_to_typesafe_for_good_and_leaves_the_marker() {
    let e = Env::new();
    let (url, rx) = fake_jev_with(
        "ignore",
        0.9,
        Fake {
            cloudflare_broke: true,
            ..Fake::default()
        },
    );
    setup(&e, &url, "");
    let (doc, err) = start(&e, BOTH);
    assert_eq!(doc["state"], "ok", "{doc}");
    assert_eq!(doc["steps"][0]["outcome"], "ignore", "{doc}");
    let paths: Vec<String> = rx.try_iter().map(|(p, _, _)| p).collect();
    assert_eq!(paths, ["/accounts/acct-1/ai/run", "/v1/systemone"]);
    assert_eq!(err.matches("switching to TypeSafe").count(), 1, "{err}");
    let day = std::fs::read_to_string(marker(&e)).unwrap();
    let day = day.trim();
    assert_eq!(day.len(), 10, "{day}");
    assert_eq!(&day[4..5], "-", "{day}");

    // A later process reads the marker and never asks Cloudflare again.
    let (doc, err) = start(&e, BOTH);
    assert_eq!(doc["state"], "ok", "{doc}");
    let paths: Vec<String> = rx.try_iter().map(|(p, _, _)| p).collect();
    assert_eq!(paths, ["/v1/systemone"]);
    assert!(!err.contains("switching"), "{err}");
}

#[test]
fn a_forced_backend_is_the_only_one_asked() {
    let e = Env::new();
    let (url, rx) = fake_jev_with(
        "ignore",
        0.9,
        Fake {
            cloudflare_broke: true,
            ..Fake::default()
        },
    );
    setup(&e, &url, "backend = \"cloudflare\"\n");
    let (doc, _) = start(&e, BOTH);
    assert_eq!(doc["state"], "failed", "{doc}");
    let paths: Vec<String> = rx.try_iter().map(|(p, _, _)| p).collect();
    assert_eq!(paths, ["/accounts/acct-1/ai/run"]);
    assert!(!marker(&e).exists());

    let e = Env::new();
    let (url, rx) = fake_jev("ignore", 0.9);
    setup(&e, &url, "backend = \"typesafe\"\n");
    let (doc, _) = start(&e, BOTH);
    assert_eq!(doc["state"], "ok", "{doc}");
    let paths: Vec<String> = rx.try_iter().map(|(p, _, _)| p).collect();
    assert_eq!(paths, ["/v1/systemone"]);
}

#[test]
fn an_overloaded_typesafe_is_retried_three_times_then_refused() {
    let e = Env::new();
    let (url, rx) = fake_jev_with(
        "ignore",
        0.9,
        Fake {
            busy: 3,
            ..Fake::default()
        },
    );
    setup(&e, &url, "");
    let (doc, _) = start(&e, TYPESAFE);
    assert_eq!(doc["state"], "ok", "{doc}");
    assert_eq!(rx.try_iter().count(), 4);

    let e = Env::new();
    let (url, rx) = fake_jev_with(
        "ignore",
        0.9,
        Fake {
            busy: 4,
            ..Fake::default()
        },
    );
    setup(&e, &url, "");
    let (doc, _) = start(&e, TYPESAFE);
    assert_eq!(doc["state"], "failed", "{doc}");
    assert_eq!(rx.try_iter().count(), 4);
    assert!(
        doc.to_string().contains("rate limited by the provider"),
        "{doc}"
    );
}
