//! Ask Forge (docs/CHAT.md): `forge chat` against a fake chat provider — a
//! loopback `/chat/completions` endpoint the test plays the model from.
//! A question about a seeded failed task is answered from the `task`
//! tool's result; a request to file work is proposed, changes nothing,
//! and files the task once the operator confirms it.

use crate::support::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A fake OpenAI-compatible endpoint. `model` maps the request's user
/// message to the JSON object the model answers with; every request body
/// is kept for the test to read.
struct FakeModel {
    base_url: String,
    seen: Arc<Mutex<Vec<Value>>>,
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Value> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let want: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")?
                    .trim()
                    .parse()
                    .ok()
            })
            .unwrap_or(0);
        if body.len() >= want {
            return serde_json::from_str(&body[..want]).ok();
        }
    }
}

fn fake_model(model: impl Fn(&str) -> Value + Send + 'static) -> FakeModel {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let Some(req) = read_request(&mut stream) else {
                continue;
            };
            let user = req["messages"][1]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            log.lock().unwrap().push(req);
            let content = model(&user).to_string();
            let body = json!({
                "choices": [{"message": {"role": "assistant", "content": content}}],
                "usage": {"prompt_tokens": 1000, "completion_tokens": 200},
            })
            .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    FakeModel { base_url, seen }
}

/// Config written before any `forge` command runs, so the operator's
/// providers table is the test's own. $10 per million tokens each way.
fn configure(e: &Env, m: &FakeModel, budget: &str) {
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        format!(
            "[providers.fake-chat]\nrunner = \"chat\"\nbase_url = \"{}\"\nmodel = \"fake\"\n\
             price_usd_per_million_input = 10.0\nprice_usd_per_million_output = 10.0\n{budget}",
            m.base_url
        ),
    )
    .unwrap();
}

/// A project on the test repo, and task 1 failed with a reason to ask about.
fn seed(e: &Env) {
    let repo = e.repo.to_str().unwrap();
    let ok = |args: &[&str]| assert!(e.forge("ok.sh", args).status.success(), "{args:?}");
    ok(&[
        "project",
        "new",
        "demo",
        "--purpose",
        "Demo.",
        "--repo",
        repo,
    ]);
    ok(&["add", repo, "write 42 to answer.txt", "--project", "demo"]);
    e.db()
        .execute(
            "UPDATE tasks SET state='failed', reason='test failed: answer.txt is missing' WHERE id=1",
            [],
        )
        .unwrap();
}

fn chat(e: &Env, args: &[&str]) -> std::process::Output {
    let mut all = vec!["chat"];
    all.extend_from_slice(args);
    e.forge("ok.sh", &all)
}

fn chat_json(e: &Env, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let o = chat(e, &all);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// The model of these tests: looks the task up, then answers from what the
/// tool returned; asked to file work, proposes it and says so.
fn scripted(user: &str) -> Value {
    let has_results = user.contains(" → ");
    let asked = user
        .split("newest message:")
        .nth(1)
        .unwrap_or(user)
        .to_string();
    if asked.contains("nucleosynthesis") {
        return if has_results {
            json!({"reply": "I have proposed filing it; confirm to file it.", "tool": "", "arguments": {}})
        } else {
            json!({"reply": "Proposing the task.", "tool": "add_task", "arguments": {
                "task": "write a note on stellar nucleosynthesis into notes.md", "project": "demo"}})
        };
    }
    if !has_results {
        return json!({"reply": "Looking at task 1.", "tool": "task", "arguments": {"id": 1}});
    }
    let reason = if user.contains("answer.txt is missing") {
        "answer.txt is missing"
    } else {
        "unknown"
    };
    json!({"reply": format!("Task 1 failed its test: {reason}."), "tool": "", "arguments": {}})
}

#[test]
fn a_question_about_a_seeded_task_is_answered_from_the_task_tool() {
    let e = Env::new();
    let m = fake_model(scripted);
    configure(&e, &m, "");
    seed(&e);

    let doc = chat_json(&e, &["why did task 1 fail"]);
    assert!(
        doc["reply"]
            .as_str()
            .unwrap()
            .contains("answer.txt is missing"),
        "{doc}"
    );
    let calls = doc["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1, "{doc}");
    assert_eq!(calls[0]["tool"], "task");
    assert_eq!(calls[0]["result"]["task"]["state"], "failed");
    assert_eq!(doc["proposals"].as_array().unwrap().len(), 0);
    assert!(doc["cost_usd"].as_f64().unwrap() > 0.0);

    // The system message carried the directive and the tool list, and the
    // operator's words came through as data.
    let seen = m.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one step to look, one to answer");
    let system = seen[0]["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("You are Ask Forge"), "{system}");
    assert!(system.contains("- retry_task:"), "{system}");
    drop(seen);

    // Every turn is recorded: the operator's, and the reply with its calls and cost.
    let c = e.db();
    let rows: Vec<(String, String, f64)> = c
        .prepare("SELECT role, tool_calls, cost_usd FROM chat_turns WHERE session=1 ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["user", "assistant"]
    );
    assert!(rows[1].1.contains("\"tool\":\"task\""), "{}", rows[1].1);
    assert!(rows[1].2 > 0.0);
}

#[test]
fn a_proposed_task_is_filed_only_when_the_operator_confirms_it() {
    let e = Env::new();
    let m = fake_model(scripted);
    configure(&e, &m, "");
    seed(&e);

    let first = chat_json(&e, &["why did task 1 fail"]);
    let session = first["session"].as_i64().unwrap().to_string();
    let doc = chat_json(
        &e,
        &[
            "--session",
            &session,
            "file a task that writes about nucleosynthesis",
        ],
    );
    let proposals = doc["proposals"].as_array().unwrap();
    assert_eq!(proposals.len(), 1, "{doc}");
    let action = proposals[0]["action"].as_str().unwrap().to_string();
    assert_eq!(proposals[0]["status"], "proposed");
    assert!(
        proposals[0]["summary"]
            .as_str()
            .unwrap()
            .contains("nucleosynthesis")
    );
    let count = || -> i64 {
        e.db()
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(), 1, "a proposal files nothing");

    // The text form prints the proposal and how to decide it.
    let text = chat(
        &e,
        &[
            "--session",
            &session,
            "file a task that writes about nucleosynthesis",
        ],
    );
    let out = String::from_utf8_lossy(&text.stdout).to_string();
    assert!(out.contains("forge chat confirm"), "{out}");
    assert_eq!(count(), 1);

    let o = chat(&e, &["confirm", &action, "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let done: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(done["status"], "confirmed", "{done}");
    assert_eq!(count(), 2, "the confirmed proposal was filed");
    let (text, state, project): (String, String, String) = e
        .db()
        .query_row(
            "SELECT task, state, project FROM tasks WHERE id=2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(text.contains("nucleosynthesis"), "{text}");
    assert_eq!((state.as_str(), project.as_str()), ("queued", "demo"));

    // Recorded as a decision, and it does not run twice.
    let decisions = e.decisions_json();
    assert!(
        decisions
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["kind"] == "chat-action" && d["task_id"] == 2),
        "{decisions}"
    );
    let again = chat(&e, &["confirm", &action]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already confirmed"));
    assert_eq!(count(), 2);

    // The session shows it all, and lists among the sessions.
    let shown =
        serde_json::from_slice::<Value>(&chat(&e, &["show", &session, "--json"]).stdout).unwrap();
    let last = shown["turns"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["role"], "action");
    assert!(
        last["text"]
            .as_str()
            .unwrap()
            .starts_with("Confirmed: filed task 2"),
        "{last}"
    );
    let sessions =
        serde_json::from_slice::<Value>(&chat(&e, &["sessions", "--json"]).stdout).unwrap();
    assert_eq!(sessions["sessions"][0]["id"].to_string(), session);
}

#[test]
fn a_rejected_proposal_files_nothing() {
    let e = Env::new();
    let m = fake_model(scripted);
    configure(&e, &m, "");
    seed(&e);
    let doc = chat_json(&e, &["file a task that writes about nucleosynthesis"]);
    let action = doc["proposals"][0]["action"].as_str().unwrap().to_string();
    let o = chat(&e, &["reject", &action]);
    assert!(o.status.success());
    let n: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert!(!chat(&e, &["confirm", &action]).status.success());
}

#[test]
fn chat_cost_counts_against_the_per_day_budget() {
    let e = Env::new();
    let m = fake_model(scripted);
    // One exchange costs $0.024 at the fake's prices; the day allows a cent.
    configure(&e, &m, "[budget]\nper_day_usd = 0.01\n");
    seed(&e);
    let first = chat(&e, &["why did task 1 fail"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let doctor = e.forge("ok.sh", &["doctor", "--json"]);
    let checks: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let spend = checks
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "spend")
        .unwrap()
        .clone();
    assert!(spend["spend_usd"].as_f64().unwrap() > 0.01, "{spend}");

    let second = chat(&e, &["and why again?"]);
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("daily budget reached"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
}

#[test]
fn a_stream_prints_each_event_as_it_happens() {
    let e = Env::new();
    let m = fake_model(scripted);
    configure(&e, &m, "");
    seed(&e);
    let o = chat(&e, &["--stream", "why did task 1 fail"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let events: Vec<Value> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kinds: Vec<&str> = events.iter().map(|v| v["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["session", "turn", "tool", "reply"], "{events:?}");
}
