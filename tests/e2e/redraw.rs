//! A held provider never holds a task another provider could run
//! (docs/ECONOMIST.md, "A held arm is re-drawn at claim time"): tasks
//! drawn to a provider whose window is spent are re-drawn to a free arm of
//! the same role at claim time, and `forge task set --provider` routes a
//! queued task by hand.

use crate::support::*;

/// A config with a second provider beside the built-in anthropic, and an
/// experiment drawing the code role between the two.
fn two_arms(e: &Env) {
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[providers.fake-codex]\nrunner = \"codex-cli\"\nmodel = \"codex-fake-model\"\n",
    )
    .unwrap();
    let dir = e.home.join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("experiment.toml"),
        "[factors.code]\nanthropic = 0.5\nfake-codex = 0.5\n",
    )
    .unwrap();
}

/// Draw `ids` to fake-codex, whatever the experiment drew them.
fn draw_to_codex(e: &Env, ids: &[i64]) {
    for id in ids {
        e.db()
            .execute(
                "UPDATE tasks SET explore_json='{\"code\":\"fake-codex\"}' WHERE id=?1",
                [id],
            )
            .unwrap();
    }
}

/// fake-codex's 5h window at 100%, resetting in an hour, as a withdrawn
/// task's finished attempt reported it.
fn spend_codex_window(e: &Env, holder: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    e.db()
        .execute(
            "INSERT INTO attempts (task_id, attempt_no, state, started_at, finished_at, provider, rl_five_hour, rl_five_hour_resets)
             VALUES (?1, 1, 'failed', ?2, ?2, 'fake-codex', 1.0, ?3)",
            rusqlite::params![holder, now, now + 3600],
        )
        .unwrap();
    e.db()
        .execute("UPDATE tasks SET state='withdrawn' WHERE id=?1", [holder])
        .unwrap();
}

#[test]
fn two_tasks_drawn_to_a_held_provider_both_start_on_a_free_one_in_one_pass() {
    let e = Env::new();
    two_arms(&e);
    let (a, b, holder) = (e.add(&["--no-land"]), e.add(&["--no-land"]), e.add(&[]));
    draw_to_codex(&e, &[a, b]);
    spend_codex_window(&e, holder);

    let o = e.cmd("ok.sh").args(["work", "--once"]).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    eprintln!("--- forge work --once ---\n{err}");
    assert!(o.status.success(), "{err}");

    for id in [a, b] {
        assert!(err.contains(&format!("task {id} starting")), "{err}");
        assert_eq!(e.task(id).0, "succeeded", "{}", e.task(id).1);
        let explore: String = e
            .db()
            .query_row("SELECT explore_json FROM tasks WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        let doc: serde_json::Value = serde_json::from_str(&explore).unwrap();
        assert_eq!(doc["code"], "anthropic", "{explore}");
        let note = doc["redraw:code"].as_str().unwrap();
        assert!(
            note.starts_with("code: fake-codex held until ")
                && note.ends_with(", re-drawn anthropic"),
            "{note}"
        );
        let provider: String = e
            .db()
            .query_row(
                "SELECT provider FROM attempts WHERE task_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(provider, "anthropic");
    }
    assert!(
        !err.contains("; holding,"),
        "nothing was left to wait for: {err}"
    );
}

#[test]
fn task_set_provider_routes_a_queued_task_by_hand() {
    let e = Env::new();
    two_arms(&e);
    let id = e.add(&["--no-land"]);

    let o = e.forge(
        "ok.sh",
        &["task", "set", &id.to_string(), "--provider", "nope"],
    );
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("unknown provider"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    let o = e.forge(
        "ok.sh",
        &["task", "set", &id.to_string(), "--provider", "fake-codex"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let provider: String = e
        .db()
        .query_row("SELECT provider FROM tasks WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(provider, "fake-codex");
}
