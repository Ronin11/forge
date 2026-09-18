//! The statusline plugin end to end: it maintains a status file a bar
//! widget can read, atomically, driven only by `forge snapshot` and
//! `forge requests --json`. The plugin writes into
//! `$XDG_STATE_HOME/forge2/status.json` (or `~/.local/state/forge2/status.json`
//! when unset), so each test points a scratch `HOME` at the spawned worker
//! rather than touching the real one.

use crate::support::*;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn enable_statusline(e: &Env) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/statusline");
    assert!(
        e.forge("ok.sh", &["plugin", "install", src.to_str().unwrap()])
            .status
            .success()
    );
    assert!(
        e.forge("ok.sh", &["plugin", "enable", "statusline"])
            .status
            .success()
    );
}

fn status_path(fake_home: &Path) -> PathBuf {
    fake_home.join(".local/state/forge2/status.json")
}

fn read_status(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn state_is(path: &Path, want: &str) -> bool {
    read_status(path)
        .and_then(|v| v.get("state").and_then(|s| s.as_str()).map(String::from))
        .as_deref()
        == Some(want)
}

#[derive(Debug, Deserialize)]
struct RunningTask {
    id: i64,
    repo: String,
    workflow: String,
    elapsed_s: i64,
}

#[derive(Debug, Deserialize)]
struct StatusDoc {
    schema: i64,
    #[allow(dead_code)]
    ts: i64,
    state: String,
    running: Vec<RunningTask>,
    queued: i64,
    blocked: i64,
    failed_recently: bool,
    questions: i64,
    #[serde(default)]
    ui: Option<String>,
}

#[test]
fn a_quiet_home_is_idle_and_a_running_task_moves_through_working_to_a_well_formed_document() {
    let e = Env::new();
    enable_statusline(&e);

    let fake_home = tempfile::tempdir().unwrap();
    let path = status_path(fake_home.path());

    let mut worker = Worker::spawn(
        e.cmd("ok.sh")
            .env("HOME", fake_home.path())
            .env("FAKE_SLEEP", "1")
            .args(["work", "--poll", "1"]),
    );

    assert!(
        wait_until(|| path.exists(), Duration::from_secs(10)),
        "expected {} to appear in a quiet home",
        path.display()
    );
    let doc: StatusDoc = serde_json::from_value(read_status(&path).unwrap()).unwrap();
    assert_eq!(doc.schema, 1);
    assert_eq!(doc.state, "idle");
    assert!(doc.running.is_empty());
    assert_eq!(doc.queued, 0);
    assert_eq!(doc.blocked, 0);
    assert!(!doc.failed_recently);
    assert_eq!(doc.questions, 0);
    assert_eq!(doc.ui, None, "FORGE_WEB_URL is unset; ui must be omitted");

    let id = e.add(&[]);

    assert!(
        wait_until(|| state_is(&path, "working"), Duration::from_secs(15)),
        "expected state to move to working: {:?}",
        read_status(&path)
    );
    let doc: StatusDoc = serde_json::from_value(read_status(&path).unwrap()).unwrap();
    assert_eq!(doc.state, "working");
    assert_eq!(doc.running.len(), 1);
    assert_eq!(doc.running[0].id, id);
    assert_eq!(
        doc.running[0].repo, "repo",
        "repo is the base name, not the path"
    );
    assert_eq!(doc.running[0].workflow, "direct");
    assert!(doc.running[0].elapsed_s >= 0);

    assert!(
        wait_until(|| e.task(id).0 == "succeeded", Duration::from_secs(15)),
        "expected the task to finish: {:?}",
        e.task(id)
    );

    worker.stop();
}

#[test]
fn a_blocked_task_moves_the_state_to_attention() {
    let e = Env::new();
    enable_statusline(&e);

    let fake_home = tempfile::tempdir().unwrap();
    let path = status_path(fake_home.path());

    let id = e.add(&[]);

    let mut worker = Worker::spawn(
        e.cmd("needsinput.sh").env("HOME", fake_home.path()).args(["work"]),
    );

    assert!(
        wait_until(|| e.task(id).0 == "blocked", Duration::from_secs(15)),
        "the task never blocked"
    );
    assert!(
        wait_until(|| state_is(&path, "attention"), Duration::from_secs(15)),
        "expected attention once a question is open: {:?}",
        read_status(&path)
    );
    let doc: StatusDoc = serde_json::from_value(read_status(&path).unwrap()).unwrap();
    assert_eq!(doc.state, "attention");
    assert_eq!(doc.blocked, 1);
    assert_eq!(doc.questions, 1);
    assert!(!doc.failed_recently);

    worker.stop();
}

/// A task can sit queued or run long enough that its total lifetime
/// exceeds an hour even though it failed seconds ago: `failed_recently`
/// must key off `finished_at`, not `created_at` (see `TaskRow::finished_at`
/// / `TaskSummary::finished_at`, src/store.rs, src/view.rs).
#[test]
fn a_task_created_long_ago_but_finished_recently_still_counts_as_recently_failed() {
    let e = Env::new();
    enable_statusline(&e);

    assert!(!e.run("crash.sh", &["--retries", "0"]).status.success());
    assert_eq!(e.task(1).0, "failed");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    e.db()
        .execute(
            "UPDATE tasks SET created_at = ?1, finished_at = ?2 WHERE id = 1",
            [now - 7200, now],
        )
        .unwrap();

    let fake_home = tempfile::tempdir().unwrap();
    let path = status_path(fake_home.path());

    let mut worker = Worker::spawn(e.cmd("ok.sh").env("HOME", fake_home.path()).args(["work"]));

    assert!(
        wait_until(|| state_is(&path, "attention"), Duration::from_secs(15)),
        "expected attention from a recently-finished failure: {:?}",
        read_status(&path)
    );
    let doc: StatusDoc = serde_json::from_value(read_status(&path).unwrap()).unwrap();
    assert_eq!(doc.state, "attention");
    assert!(doc.failed_recently);
    assert_eq!(doc.questions, 0, "nothing is blocked in this scenario");

    worker.stop();
}
