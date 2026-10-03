//! A run workflow's operation step given a named secret and a declared
//! egress host (docs/JOBS.md, "Secrets and egress on a step"): the
//! operation sees the secret and reaches the fake endpoint on its
//! allowlist, and nothing Forge records holds the secret's value.

use crate::support::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

const SECRET: &str = "cf-tok-7f3a91c2d5e84b60";

/// A fake HTTP endpoint on 127.0.0.1 that answers every request `200 ok`
/// and remembers what it was sent.
struct Endpoint {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

fn endpoint() -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..n]).into_owned());
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    Endpoint { port, seen }
}

const PLAY: &str = r#"name = "play-segments"
kind = "operation"
description = "calls the endpoint with the secret, then leaks it on purpose into every place a step writes"
run = ["bash", "-c", '''
set -e
resp=$(curl --fail --max-time 5 --noproxy "" -sS -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" "$FORGE_INPUT_URL")
echo "token is $CLOUDFLARE_API_TOKEN"
echo "endpoint said $resp" >&2
printf "row\treport.csv\tauth=%s\n" "$CLOUDFLARE_API_TOKEN" >> "$FORGE_EFFECT_LOG"
echo "$FORGE_INPUT_COST" > "$FORGE_COST_FILE"
''']
"#;

const PROBE: &str = r#"name = "probe-env"
kind = "operation"
description = "records whether the secret is in its environment"
run = ["bash", "-c", '''
printf "row\tprobe.csv\tsecret=%s\n" "${CLOUDFLARE_API_TOKEN:-absent}" >> "$FORGE_EFFECT_LOG"
''']
"#;

/// A project with the `[secrets]` table, the two actions and a workflow
/// whose first step declares the secret, the endpoint's host and a budget.
fn setup(e: &Env, port: u16, limit_usd: f64, cost: &str) {
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "exploration",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("config.toml"),
        "[secrets]\ncloudflare_token = { env = \"CLOUDFLARE_API_TOKEN\" }\n",
    )
    .unwrap();
    for (name, text) in [("play-segments", PLAY), ("probe-env", PROBE)] {
        std::fs::write(e.home.join(format!("workflows/actions/{name}.toml")), text).unwrap();
    }
    std::fs::write(
        e.home.join("workflows/nightly-jev.toml"),
        format!(
            r#"name = "nightly-jev"
kind = "run"
description = "plays the segments through the endpoint and keeps the report"

steps = [
  {{ action = "play-segments", effect = "row", secrets = ["cloudflare_token"], egress = ["127.0.0.1:{port}"], budget_usd = 0.25 }},
  {{ action = "probe-env", effect = "row" }},
]

[trigger]
on = "manual"

[limits]
budget_usd = {limit_usd}
per_day = 10
on_failure = "drop"
"#
        ),
    )
    .unwrap();
    std::fs::write(
        e.home.join("input.json"),
        format!(r#"{{"url":"http://127.0.0.1:{port}/segments","cost":"{cost}"}}"#),
    )
    .unwrap();
}

/// `forge job start exploration nightly-jev --now`, the worker's
/// environment holding the secret; the job's `show --json` document.
fn run_job(e: &Env) -> serde_json::Value {
    let input = e.home.join("input.json");
    let o = e
        .cmd("ok.sh")
        .env("CLOUDFLARE_API_TOKEN", SECRET)
        .args(["job", "start", "exploration", "nightly-jev", "--input"])
        .arg(&input)
        .arg("--now")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!String::from_utf8_lossy(&o.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&o.stderr).contains(SECRET));
    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let shown = e.forge("ok.sh", &["job", "show", &id, "--json"]);
    serde_json::from_slice(&shown.stdout).unwrap()
}

/// Every file under `dir` whose bytes hold `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_holding(&path, needle));
        } else if std::fs::read(&path)
            .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
        {
            found.push(path.display().to_string());
        }
    }
    found
}

#[test]
fn a_step_sees_its_declared_secret_and_reaches_its_host_and_nothing_records_the_value() {
    let e = Env::new();
    if e.sandbox_disabled() {
        return;
    }
    let fake = endpoint();
    setup(&e, fake.port, 1.0, "0.1");
    let doc = run_job(&e);
    assert_eq!(doc["state"], "ok", "{doc:?}");

    // The operation saw the secret and reached the fake endpoint with it.
    let seen = fake.seen.lock().unwrap().join("\n");
    assert!(seen.contains(&format!("Bearer {SECRET}")), "{seen}");

    // The step's tail, its output file and its effect carry the placeholder.
    let steps = doc["steps"].as_array().unwrap();
    let tail = steps[0]["tail"].as_str().unwrap();
    assert!(
        tail.contains("token is [redacted:cloudflare_token]"),
        "{tail}"
    );
    assert!(tail.contains("endpoint said ok"), "{tail}");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects[0]["summary"], "auth=[redacted:cloudflare_token]");

    // The second step declared nothing and was given nothing.
    assert_eq!(effects[1]["summary"], "secret=absent", "{effects:?}");

    // Nothing under the home directory (store, logs, events, step
    // output, effect log) holds the value; the show document doesn't.
    assert!(!doc.to_string().contains(SECRET));
    let leaks = files_holding(&e.home, SECRET);
    assert!(leaks.is_empty(), "{leaks:?}");
    let text =
        String::from_utf8_lossy(&e.forge("ok.sh", &["job", "show", "1"]).stdout).into_owned();
    assert!(!text.contains(SECRET), "{text}");
    let log = e.forge("ok.sh", &["job", "log", "1"]);
    assert!(!String::from_utf8_lossy(&log.stdout).contains(SECRET));
}

#[test]
fn a_steps_reported_cost_counts_against_the_jobs_budget() {
    let e = Env::new();
    if e.sandbox_disabled() {
        return;
    }
    let fake = endpoint();
    setup(&e, fake.port, 1.0, "0.1");
    let doc = run_job(&e);
    assert_eq!(doc["state"], "ok", "{doc:?}");
    assert_eq!(doc["cost_usd"], 0.1, "{doc:?}");
    assert_eq!(doc["steps"][0]["cost_usd"], 0.1, "{doc:?}");
}

#[test]
fn a_step_that_spends_past_the_jobs_budget_asks_the_operator() {
    let e = Env::new();
    if e.sandbox_disabled() {
        return;
    }
    let fake = endpoint();
    setup(&e, fake.port, 0.05, "0.1");
    let doc = run_job(&e);
    assert_eq!(doc["state"], "needs_human", "{doc:?}");
    assert_eq!(doc["cost_usd"], 0.1, "{doc:?}");
    assert_eq!(doc["steps"].as_array().unwrap().len(), 1, "{doc:?}");
}

#[test]
fn a_step_that_reports_more_than_it_declared_fails() {
    let e = Env::new();
    if e.sandbox_disabled() {
        return;
    }
    let fake = endpoint();
    setup(&e, fake.port, 5.0, "0.5");
    let doc = run_job(&e);
    assert_eq!(doc["state"], "failed", "{doc:?}");
    let tail = doc["steps"][0]["tail"].as_str().unwrap();
    assert!(tail.contains("over its declared budget_usd"), "{tail}");
}

#[test]
fn a_secret_the_config_does_not_define_fails_the_step_without_running_it() {
    let e = Env::new();
    let fake = endpoint();
    setup(&e, fake.port, 1.0, "0.1");
    std::fs::write(e.home.join("config.toml"), "# no secrets\n").unwrap();
    let doc = run_job(&e);
    assert_eq!(doc["state"], "failed", "{doc:?}");
    assert!(fake.seen.lock().unwrap().is_empty());
    assert!(!doc.to_string().contains(SECRET));
}

/// A message trigger's job carries contact trust, never operator trust
/// (`job::start_message`, `store::set_job_trust`); a step declaring
/// `secrets` is refused before it runs, so a stranger who gets a run
/// workflow to fire can never see, nor leak, an operator's secret.
#[test]
fn a_message_triggered_jobs_step_never_sees_the_operators_secret() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "exploration",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("config.toml"),
        "[secrets]\ncloudflare_token = { env = \"CLOUDFLARE_API_TOKEN\" }\n",
    )
    .unwrap();
    std::fs::write(e.home.join("workflows/actions/probe-env.toml"), PROBE).unwrap();
    std::fs::write(
        e.home.join("workflows/nightly-jev.toml"),
        r#"name = "nightly-jev"
kind = "run"
description = "a stranger's message must never be trusted with a secret"

steps = [
  { action = "probe-env", effect = "row", secrets = ["cloudflare_token"] },
]

[trigger]
on = "message"
contact = "*"
"#,
    )
    .unwrap();
    let secret = "operator-only-secret";
    let o = e.forge(
        "ok.sh",
        &[
            "message",
            "record",
            "exploration",
            "--channel",
            "signal",
            "--from",
            "stranger",
            "--text",
            "hi",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e
        .cmd("ok.sh")
        .env("CLOUDFLARE_API_TOKEN", secret)
        .args(["work", "--once"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let id = rows[0]["id"].as_i64().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    // The step was refused before it ran; the job never got the secret.
    assert_eq!(doc["state"], "failed", "{doc:?}");
    assert!(doc["steps"].as_array().unwrap().is_empty(), "{doc:?}");
    let verdict = doc["verdict_json"].as_str().unwrap();
    assert!(
        verdict.contains("only an operator-trust job may be given"),
        "{verdict}"
    );

    assert!(!doc.to_string().contains(secret));
    let leaks = files_holding(&e.home, secret);
    assert!(leaks.is_empty(), "{leaks:?}");
    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["job", "show", &id.to_string()]).stdout)
        .into_owned();
    assert!(!text.contains(secret), "{text}");
    let log = e.forge("ok.sh", &["job", "log", &id.to_string()]);
    assert!(!String::from_utf8_lossy(&log.stdout).contains(secret));
}

#[test]
fn a_declared_secret_cannot_reach_an_undeclared_endpoint() {
    for bypass_proxy in [false, true] {
        let e = Env::new();
        if e.sandbox_disabled() {
            return;
        }
        let fake = endpoint();
        setup(&e, fake.port, 1.0, "0.1");
        let workflow = e.home.join("workflows/nightly-jev.toml");
        let text = std::fs::read_to_string(&workflow).unwrap();
        std::fs::write(
            &workflow,
            text.replace(&format!("127.0.0.1:{}", fake.port), "api.cloudflare.com"),
        )
        .unwrap();
        if bypass_proxy {
            std::fs::write(
                e.home.join("workflows/actions/play-segments.toml"),
                PLAY.replace("--noproxy \"\"", "--noproxy '*'"),
            )
            .unwrap();
        }
        let doc = run_job(&e);
        assert_eq!(doc["state"], "failed", "{doc:?}");
        let tail = doc["steps"][0]["tail"].as_str().unwrap();
        if bypass_proxy {
            assert!(tail.contains("Failed to connect"), "{tail}");
        } else {
            assert!(tail.contains("403"), "{tail}");
        }
        assert!(fake.seen.lock().unwrap().is_empty());
        assert!(!doc.to_string().contains(SECRET));
        assert!(files_holding(&e.home, SECRET).is_empty());
    }
}

#[test]
fn a_secret_step_is_refused_when_network_isolation_is_disabled() {
    let e = Env::new();
    let fake = endpoint();
    setup(&e, fake.port, 1.0, "0.1");
    let out = e
        .cmd("ok.sh")
        .env("FORGE_SANDBOX", "0")
        .env("CLOUDFLARE_API_TOKEN", SECRET)
        .args([
            "job",
            "start",
            "exploration",
            "nightly-jev",
            "--now",
            "--input",
        ])
        .arg(e.home.join("input.json"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let shown = e.forge("ok.sh", &["job", "show", &id, "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(doc["state"], "failed", "{doc:?}");
    assert!(
        doc.to_string().contains("network-isolating executor"),
        "{doc:?}"
    );
    assert!(fake.seen.lock().unwrap().is_empty());
    assert!(!String::from_utf8_lossy(&out.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&out.stderr).contains(SECRET));
    assert!(files_holding(&e.home, SECRET).is_empty());
}
