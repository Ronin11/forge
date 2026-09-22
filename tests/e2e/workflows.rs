use crate::support::*;
use forge_client::{WorkflowLintProblem, WorkflowShowDoc};
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

/// `forge workflows validate`: a repository check on its own
/// `.forge/workflows/`, with no store and no FORGE2_HOME so it runs
/// wherever the `forge` binary does (docs/WORKFLOWS.md). Catches the
/// equitizr shape (a string `trigger`) that landed twice because nothing
/// ran the catalog's own loader against the repository's own files.
#[test]
fn forge_workflows_validate_runs_with_no_store_and_no_forge2_home() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    std::fs::create_dir_all(repo.join(".forge/workflows")).unwrap();
    std::fs::write(
        repo.join(".forge/workflows/publish-snapshot.toml"),
        "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"d\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n\n[trigger]\non = \"manual\"\n",
    )
    .unwrap();

    let o = std::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .env_remove("FORGE2_HOME")
        .args(["workflows", "validate", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim(),
        "1 workflow(s), 0 action(s) valid"
    );

    std::fs::write(
        repo.join(".forge/workflows/publish-snapshot.toml"),
        "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented string trigger\"\ntrigger = \"manual\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n",
    )
    .unwrap();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .env_remove("FORGE2_HOME")
        .args(["workflows", "validate", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("publish-snapshot.toml:4:"),
        "expected file and line: {out}"
    );
}

/// `forge workflows show NAME --json`: a catalog workflow in full — its
/// file text, source, kind, every resolved step with the action's name,
/// kind, contract, model, turns, timeout and description, and its
/// measured profile in the same shape `forge workflows --json` computes.
#[test]
fn forge_workflows_show_prints_a_catalog_workflow_in_full() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let o = e.forge("ok.sh", &["workflows", "show", "direct", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["name"], "direct");
    assert_eq!(doc["source"], "catalog");
    assert_eq!(doc["kind"], "build");
    assert!(
        doc["text"]
            .as_str()
            .unwrap()
            .contains("one agent writes the change"),
        "{doc}"
    );
    let steps = doc["steps"].as_array().unwrap();
    let names: Vec<&str> = steps.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["setup", "repo-map", "code"]);
    let code = steps.iter().find(|s| s["name"] == "code").unwrap();
    assert_eq!(code["kind"], "directive");
    assert_eq!(code["contract"], "code");
    assert!(!code["description"].as_str().unwrap().is_empty(), "{doc}");
    // steps carries max_turns/timeout_secs/model keys even when unset by
    // this action, so a client never has to guess whether they're absent.
    assert!(code.get("max_turns").is_some());
    assert!(code.get("timeout_secs").is_some());
    assert_eq!(doc["measured"]["current"]["known"], false);
    assert_eq!(doc["measured"]["regressed"], false);

    let o = e.forge("ok.sh", &["workflows", "show", "no-such-workflow"]);
    assert!(!o.status.success());
}

/// `forge workflows show NAME --project P --json`: a run workflow that
/// lives in a project's own repository rather than the operator catalog
/// (docs/JOBS.md, "Where an automation lives") — `source` says so, and its
/// steps still resolve against the operator's built-in actions.
#[test]
fn forge_workflows_show_finds_a_run_workflow_in_a_projects_repository() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "equitizr",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );
    std::fs::create_dir_all(e.repo.join(".forge/workflows")).unwrap();
    std::fs::write(
        e.repo.join(".forge/workflows/publish-snapshot.toml"),
        r#"name = "publish-snapshot"
kind = "run"
description = "publishes equitizr's snapshot"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"
"#,
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &["commit", "-qm", "add publish-snapshot automation"],
    );

    let o = e.forge(
        "ok.sh",
        &[
            "workflows",
            "show",
            "publish-snapshot",
            "--project",
            "equitizr",
            "--json",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["name"], "publish-snapshot");
    assert_eq!(doc["source"], "repo");
    assert_eq!(doc["kind"], "run");
    let steps = doc["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["name"], "write-file");
    assert_eq!(steps[0]["kind"], "operation");
}

/// `forge workflows lint --stdin`: a candidate file that resolves cleanly
/// against the catalog prints no problems and exits 0.
#[test]
fn forge_workflows_lint_a_clean_candidate_exits_zero() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let text = "name = \"candidate\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n";
    let o = e.forge_stdin("ok.sh", &["workflows", "lint", "--stdin"], text);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["problems"].as_array().unwrap().len(), 0, "{doc}");
}

/// `forge workflows lint --stdin`: a candidate naming two unknown actions
/// is two lint problems, not a crash and not just the first — each on its
/// own line, each message naming its own action — and lint writes nothing
/// into a fresh `FORGE2_HOME`, unlike every other catalog command.
#[test]
fn forge_workflows_lint_reports_an_unknown_action() {
    let e = Env::new();
    // A wholly empty FORGE2_HOME: no prior catalog command has written
    // anything into it, so lint must resolve against the built-ins alone
    // and still must not write anything itself.
    let text = "name = \"candidate\"\ndescription = \"d\"\nsteps = [\n{ action = \"missing-one\" },\n{ action = \"missing-two\" },\n]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n";
    let o = e.forge_stdin("ok.sh", &["workflows", "lint", "--stdin"], text);
    assert!(!o.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let problems = doc["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 2, "{doc}");
    assert_eq!(problems[0]["line"], 4, "{doc}");
    assert!(
        problems[0]["message"]
            .as_str()
            .unwrap()
            .contains("unknown action \"missing-one\""),
        "{doc}"
    );
    assert_eq!(problems[1]["line"], 5, "{doc}");
    assert!(
        problems[1]["message"]
            .as_str()
            .unwrap()
            .contains("unknown action \"missing-two\""),
        "{doc}"
    );
    assert!(
        holds_no_files(&e.home),
        "lint must not write into FORGE2_HOME: {:?}",
        std::fs::read_dir(&e.home).map(|d| d
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect::<Vec<_>>())
    );

    // A --name that disagrees with the candidate's own `name` is refused
    // the same way a real file's name mismatch is (`parse_workflow`).
    let o = e.forge_stdin(
        "ok.sh",
        &["workflows", "lint", "--stdin", "--name", "other"],
        text,
    );
    assert!(!o.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        doc["problems"][0]["message"]
            .as_str()
            .unwrap()
            .contains("does not match the file name"),
        "{doc}"
    );
}

/// `forge-client`'s own typed `WorkflowShowDoc`/`WorkflowLintProblem`
/// parse what `forge workflows show --json`/`forge workflows lint --stdin`
/// actually print, the same way `forge_client_parses_trace_snapshot_log_and_requests`
/// (tests/e2e/listing.rs) checks the client crate's other types against
/// live output rather than a hand-maintained fixture.
#[test]
fn forge_client_parses_workflow_show_and_lint() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());

    let show: WorkflowShowDoc = serde_json::from_slice(
        &e.forge("ok.sh", &["workflows", "show", "direct", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(show.name, "direct");
    assert_eq!(show.source, "catalog");
    assert_eq!(show.kind, "build");
    let names: Vec<&str> = show.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["setup", "repo-map", "code"]);

    let text = "name = \"candidate\"\ndescription = \"d\"\nsteps = [{ action = \"nope\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n";
    let o = e.forge_stdin("ok.sh", &["workflows", "lint", "--stdin"], text);
    assert!(!o.status.success());
    let problems: Vec<WorkflowLintProblem> = serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["problems"].clone(),
    )
    .unwrap();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("unknown action \"nope\""));
}

/// `forge workflows put NAME --stdin --message TEXT`: a clean candidate
/// lands in `<FORGE2_HOME>/workflows/NAME.toml`, committed in the
/// catalog's own git with the given message, and the printed hash is
/// that commit — the same document `forge workflows show` then reads.
#[test]
fn forge_workflows_put_lands_in_the_catalog_with_a_commit() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let text = "name = \"put-me\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n";
    let o = e.forge_stdin(
        "ok.sh",
        &[
            "workflows",
            "put",
            "put-me",
            "--stdin",
            "--message",
            "add put-me",
        ],
        text,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let hash = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert_eq!(hash.len(), 40, "expected a git commit hash: {hash:?}");

    let dir = e.home.join("workflows");
    assert_eq!(
        std::fs::read_to_string(dir.join("put-me.toml")).unwrap(),
        text
    );
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), hash);
    assert_eq!(
        git(&dir, &["log", "-1", "--format=%s", &hash]),
        "add put-me"
    );
    let files = git(&dir, &["show", "--name-only", "--format=", &hash]);
    assert_eq!(
        files, "put-me.toml",
        "put must commit only the file it wrote: {files}"
    );

    let show: WorkflowShowDoc = serde_json::from_slice(
        &e.forge("ok.sh", &["workflows", "show", "put-me", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(show.name, "put-me");
    assert_eq!(show.source, "catalog");
}

/// `forge workflows put`: a candidate that fails lint, a NAME that
/// disagrees with the candidate's own declared `name`, and an empty
/// `--message` are each refused without writing anything into the
/// catalog.
#[test]
fn forge_workflows_put_refuses_a_bad_file_and_writes_nothing() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());

    // Fails lint: an unknown action reference.
    let bad = "name = \"bad\"\ndescription = \"d\"\nsteps = [{ action = \"missing\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n";
    let o = e.forge_stdin(
        "ok.sh",
        &["workflows", "put", "bad", "--stdin", "--message", "m"],
        bad,
    );
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("unknown action")
            || String::from_utf8_lossy(&o.stdout).contains("unknown action"),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!e.home.join("workflows/bad.toml").exists());

    // NAME does not match the candidate's own declared name.
    let good = "name = \"good\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n";
    let o = e.forge_stdin(
        "ok.sh",
        &["workflows", "put", "other", "--stdin", "--message", "m"],
        good,
    );
    assert!(!o.status.success());
    assert!(!e.home.join("workflows/other.toml").exists());
    assert!(!e.home.join("workflows/good.toml").exists());

    // Empty message.
    let o = e.forge_stdin(
        "ok.sh",
        &["workflows", "put", "good", "--stdin", "--message", ""],
        good,
    );
    assert!(!o.status.success());
    assert!(!e.home.join("workflows/good.toml").exists());
}

/// `forge workflows put NAME --stdin --message TEXT --repo PATH`: instead
/// of writing the catalog, it files a direct task on the repository's
/// project whose text carries the candidate content for
/// `.forge/workflows/NAME.toml`, printing the new task's id.
#[test]
fn forge_workflows_put_repo_files_a_task() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let text = "name = \"repofile\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { action = \"code\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n";
    let o = e.forge_stdin(
        "ok.sh",
        &[
            "workflows",
            "put",
            "repofile",
            "--stdin",
            "--message",
            "m",
            "--repo",
            e.repo.to_str().unwrap(),
        ],
        text,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id_s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let id: i64 = id_s
        .parse()
        .unwrap_or_else(|_| panic!("not a task id: {id_s:?}"));

    // Nothing was written to the catalog.
    assert!(!e.home.join("workflows/repofile.toml").exists());

    let log: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["log", "--json"]).stdout).unwrap();
    let row = log
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"].as_i64() == Some(id))
        .unwrap_or_else(|| panic!("task {id} not in log: {log}"));
    assert_eq!(row["state"], "queued", "{row}");
    let task_text = row["text"].as_str().unwrap();
    assert!(
        task_text.contains(".forge/workflows/repofile.toml"),
        "{task_text}"
    );
    assert!(task_text.contains(text), "{task_text}");
}
