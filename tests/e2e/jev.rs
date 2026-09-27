//! docs/EXECUTION.md, "The judgment tier": a directive step judged by a
//! `jev` provider against a fake Workers AI endpoint answering the
//! documented shape.

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

/// A loopback endpoint answering each request with a Jev result for
/// `choice` at `confidence`; the receiver carries (path, authorization,
/// body) per request.
fn fake_jev(
    choice: &'static str,
    confidence: f64,
) -> (String, Receiver<(String, String, serde_json::Value)>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let mut body = String::new();
            std::io::Read::read_to_string(req.as_reader(), &mut body).unwrap();
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.to_string())
                .unwrap_or_default();
            let _ = tx.send((
                req.url().to_string(),
                auth,
                serde_json::from_str(&body).unwrap(),
            ));
            let other = if choice == "reply" { "ignore" } else { "reply" };
            let answer = serde_json::json!({"result": {"result": {
                "model": "jev-1.13.0",
                "answers": {"outcome": {
                    "type": "choice", "choice": choice, "confidence": confidence,
                    "probabilities": {choice: confidence, other: 1.0 - confidence},
                    "legend": {},
                }},
                "usage": {"input_tokens": 500, "output_tokens": 12},
            }}, "success": true});
            let _ = req.respond(tiny_http::Response::from_string(answer.to_string()));
        }
    });
    (url, rx)
}

fn judged_run(e: &Env, url: &str) -> serde_json::Value {
    let repo_s = e.repo.to_str().unwrap();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        format!(
            "[providers.jev]\nrunner = \"jev\"\nbase_url = \"{url}/accounts/{{account_id}}/ai/run\"\n[roles]\nplan = \"jev\"\n"
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
    let o = e
        .cmd("ok.sh")
        .env("CLOUDFLARE_ACCOUNT_ID", "acct-1")
        .env("CLOUDFLARE_API_TOKEN", "tok-1")
        .args(["job", "start", "equitizr", "triage-flow", "--now"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let id: i64 = out
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("{out}{}", String::from_utf8_lossy(&o.stderr)));
    let shown = e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"]);
    serde_json::from_slice(&shown.stdout).unwrap()
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
    assert_eq!(path, "/accounts/acct-1/ai/run");
    assert_eq!(auth, "Bearer tok-1");
    assert_eq!(body["model"], "typesafe/jev");
    assert_eq!(
        body["input"]["questions"]["outcome"]["criteria"]["reply"],
        "a person needs an answer"
    );
    assert!(
        body["input"]["state"]
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
