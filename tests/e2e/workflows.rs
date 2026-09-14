use crate::support::*;
use std::time::Duration;

#[test]
fn a_broken_workflow_file_fails_doctor_and_blocks_task_creation() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/broken.toml"),
        "name = \"broken\"\nsteps = [{ kind = \"deploy\" }]\n",
    )
    .unwrap();
    let o = e.forge("ok.sh", &["doctor"]);
    assert!(!o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("FAIL workflows"), "{out}");
    assert!(out.contains("broken.toml"), "{out}");
    let o = e.forge("ok.sh", &["add", e.repo.to_str().unwrap(), "x"]);
    assert!(
        !o.status.success(),
        "a broken directory blocks task creation: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    std::fs::remove_file(e.home.join("workflows/broken.toml")).unwrap();
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("WARN workflows") && out.contains("uncommitted"),
        "{out}"
    );
}

#[test]
fn a_directives_prompt_field_lands_as_a_final_section_of_the_role_prompt() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/actions/promptcode.toml"),
        "name = \"promptcode\"\nkind = \"directive\"\ncontract = \"code\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nproduces = [\"branch\"]\nprompt = \"Write the answer in decimal, never hex.\"\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/prompted.toml"),
        "name = \"prompted\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"promptcode\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let o = e.forge(
        "promptdump.sh",
        &[
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "prompted",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let log = e.log_text(1, 1);
    assert!(log.contains("This step:"), "{log}");
    assert!(
        log.contains("Write the answer in decimal, never hex."),
        "{log}"
    );
}

#[test]
fn inline_composition_runs_the_child_and_records_every_pin() {
    let e = Env::new();
    tdd_repo(&e);
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/outer.toml"),
        "name = \"outer\"\ndescription = \"d\"\nsteps = [{ workflow = \"tdd\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
    )
    .unwrap();
    let mut c = e.with_role("ok.sh", "TESTS", "testwriter.sh");
    let o = c
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "make answer.txt contain 42",
            "--workflow",
            "outer",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = e.trace_json("1");
    let names: Vec<&str> = doc["resolved"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["action"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["tests", "setup", "repo-map", "code"]);
    assert_eq!(
        doc["resolved"]["steps"][0]["via"],
        serde_json::json!(["outer", "tdd"])
    );
    let pins: Vec<(&str, &str)> = doc["resolved"]["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["kind"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        pins,
        vec![
            ("workflow", "outer"),
            ("workflow", "tdd"),
            ("action", "tests"),
            ("action", "setup"),
            ("action", "repo-map"),
            ("action", "code")
        ]
    );
}

#[test]
fn a_resumed_task_keeps_the_versions_it_resolved() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    // Resolve by starting a task whose agent commits then dies, so it is left running.
    let mut child = e
        .cmd("hang.sh")
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "x",
            "--retries",
            "0",
            "--timeout-secs",
            "600",
        ])
        .spawn()
        .unwrap();
    assert!(
        wait_until(
            || {
                e.db()
                    .query_row("SELECT actions_json FROM tasks WHERE id=1", [], |r| {
                        r.get::<_, String>(0)
                    })
                    .map(|s| s.contains("\"pins\""))
                    .unwrap_or(false)
            },
            Duration::from_secs(20)
        ),
        "resolution was never recorded at start"
    );
    let pins_before: String = e
        .db()
        .query_row("SELECT actions_json FROM tasks WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    // Change the code action after the task resolved it.
    let code = e.home.join("workflows/actions/code.toml");
    std::fs::write(
        &code,
        std::fs::read_to_string(&code).unwrap() + "max_turns = 7\n",
    )
    .unwrap();
    // The worker is dead: the next worker requeues and resumes from the recorded resolution.
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    let pins_after: String = e
        .db()
        .query_row("SELECT actions_json FROM tasks WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(pins_before, pins_after, "a running task never re-resolves");
    let doc: serde_json::Value = e.trace_json("1");
    assert_eq!(doc["task"]["state"], "succeeded");
    let inputs_turns = doc["attempts"].as_array().unwrap().last().unwrap()["inputs"]["max_turns"]
        .as_i64()
        .unwrap();
    assert_ne!(
        inputs_turns, 7,
        "the edited file did not reach the running task"
    );
    // A new task picks up the edit.
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    let doc: serde_json::Value = e.trace_json("2");
    assert_eq!(doc["attempts"][0]["inputs"]["max_turns"], 7);
    let pins_new: String = e
        .db()
        .query_row("SELECT actions_json FROM tasks WHERE id=2", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(
        pins_before, pins_new,
        "the new task resolved the new version"
    );
}

#[test]
fn broken_references_are_refused_at_creation_and_reported_by_doctor() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/loop-a.toml"),
        "name = \"loop-a\"\nsteps = [{ workflow = \"loop-b\" }]\n",
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/loop-b.toml"),
        "name = \"loop-b\"\nsteps = [{ workflow = \"loop-a\" }]\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &["add", e.repo.to_str().unwrap(), "x", "--workflow", "loop-a"],
    );
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("references itself"));
    std::fs::write(
        e.home.join("workflows/ghost.toml"),
        "name = \"ghost\"\nsteps = [{ action = \"nope\" }]\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &["add", e.repo.to_str().unwrap(), "x", "--workflow", "direct"],
    );
    assert!(
        !o.status.success(),
        "a broken directory blocks every task, not just the broken workflow"
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("FAIL workflows"), "{out}");
    assert!(
        out.contains("references itself") || out.contains("unknown action"),
        "{out}"
    );
}

#[test]
fn a_directory_broken_after_queueing_stops_the_worker_and_keeps_the_task() {
    let e = Env::new();
    let id = e.add(&[]);
    std::fs::write(
        e.home.join("workflows/actions/code.toml"),
        "name = \"code\"\nkind = \"directive\"\nrun = [\"x\"]\n",
    )
    .unwrap();
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(!o.status.success(), "the worker stops");
    assert!(String::from_utf8_lossy(&o.stderr).contains("workflow directory is broken"));
    assert_eq!(e.task(id).0, "queued", "the task is not blamed");
}

#[test]
fn a_workflow_becomes_measured_after_enough_runs_and_regressions_are_seen() {
    let e = Env::new();
    for _ in 0..5 {
        assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    }
    let o = e.forge("ok.sh", &["workflows"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("5 run(s): verified 5/5 (100%"), "{out}");
    let doc: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["workflows", "--json"]).stdout).unwrap();
    let direct = doc["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "direct")
        .unwrap();
    assert_eq!(direct["measured"]["current"]["known"], true);
    assert_eq!(direct["measured"]["current"]["n"], 5);
    assert!(
        (direct["measured"]["current"]["cost_per_task"]
            .as_f64()
            .unwrap()
            - 0.01)
            .abs()
            < 1e-9
    );
    // A new version of direct that fails every time is a regression.
    let path = e.home.join("workflows/direct.toml");
    std::fs::write(&path, std::fs::read_to_string(&path).unwrap() + "# v2\n").unwrap();
    for _ in 0..5 {
        assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    }
    let o = e.forge("ok.sh", &["workflows"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("verified 0/5"), "{out}");
    assert!(out.contains("REGRESSION"), "{out}");
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("WARN learning") && out.contains("direct regressed"),
        "{out}"
    );
}
