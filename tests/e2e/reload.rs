//! Config reloads between claims (docs/OPS.md, "The running binary"): a
//! provider added to `config.toml` while the worker runs is drawn by the
//! next task with no restart, and an edit that fails validation is
//! ignored, with the error on the record and in doctor's config row.

use crate::support::*;
use std::path::Path;
use std::time::Duration;

fn record(e: &Env) -> serde_json::Value {
    std::fs::read_to_string(e.home.join("worker.config.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null)
}

#[test]
fn a_provider_added_while_the_worker_runs_is_drawn_without_a_restart_and_a_bad_edit_is_ignored() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    let config = e.home.join("config.toml");
    std::fs::write(&config, "[budget]\nper_task_usd = 2.0\n").unwrap();
    let worker = Worker::spawn(
        e.cmd("ok.sh")
            .env(
                "FORGE_CODEX_BIN",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/codex-ok.sh"),
            )
            .stderr(std::process::Stdio::piped())
            .args(["work", "--poll", "1"]),
    );
    assert!(
        wait_until(|| record(&e)["pid"].is_number(), Duration::from_secs(30)),
        "the worker never recorded the config it loaded"
    );

    // Added while the worker runs: the next task draws it.
    std::fs::write(
        &config,
        "[budget]\nper_task_usd = 2.0\n\n[providers.fake-codex]\nrunner = \"codex-cli\"\nmodel = \"codex-fake-model\"\n",
    )
    .unwrap();
    let id = e.add(&["--provider", "fake-codex", "--retries", "0"]);
    assert!(
        wait_until(
            || matches!(e.task(id).0.as_str(), "succeeded" | "failed"),
            Duration::from_secs(90)
        ),
        "task {id} never finished: {:?}",
        e.task(id)
    );
    let (state, reason, _) = e.task(id);
    assert_eq!(state, "succeeded", "{reason}");
    let provider: String = e
        .db()
        .query_row(
            "SELECT provider FROM attempts WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(provider, "fake-codex");

    // An edit that fails validation is ignored, with the error on the record.
    std::fs::write(&config, "[roles]\ncode = \"no-such-provider\"\n").unwrap();
    assert!(
        wait_until(
            || record(&e)["rejected"]["error"]
                .as_str()
                .is_some_and(|m| m.contains("no-such-provider")),
            Duration::from_secs(30)
        ),
        "the bad edit was never recorded: {}",
        record(&e)
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    let row = out
        .lines()
        .find(|l| l.contains("config") && l.contains("no-such-provider"))
        .unwrap_or_else(|| panic!("no config row naming the error:\n{out}"));
    assert!(row.contains("is newer than the config worker"), "{row}");
    // Two more polls go by: the refusal is logged once, not on each.
    std::thread::sleep(Duration::from_secs(3));
    let out = worker.stop_with_output();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr
            .matches("rejected, the previous config stays in force")
            .count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains("no-such-provider"), "{stderr}");
}
