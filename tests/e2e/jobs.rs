use crate::support::*;

/// A job the store recorded — no executor yet, so this simulates what
/// `store::Store::create_job`/`append_job_step`/`append_job_effect`/
/// `finish_job` would have written — shows up in `forge job list --json`,
/// `forge job show --json` and `forge job log --json` (see docs/JOBS.md,
/// "The record").
#[test]
fn a_job_inserted_by_the_store_appears_in_forge_job_list_show_and_log() {
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

    // No jobs yet.
    let empty: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    assert_eq!(empty.as_array().unwrap().len(), 0);

    let job_id = {
        let db = e.db();
        db.execute(
            "INSERT INTO jobs (project, workflow, workflow_hash, landed_sha, trigger_kind, trigger_ref, state, dry_run, started_at, finished_at, cost_usd, verdict_json)
             VALUES ('equitizr', 'quote-by-text', 'deadbeef', 'cafef00d', 'manual', '', 'ok', 0, 100, 130, 0.02, '[{\"level\":\"L0\",\"name\":\"quoted\",\"ok\":true}]')",
            [],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO job_steps (job_id, seq, action, kind, provider, model, cost_usd, started_at, finished_at, exit_code, output_ref)
             VALUES (?1, 0, 'extract-job', 'directive', 'anthropic', 'haiku', 0.001, 100, 110, NULL, 'step-0.json')",
            [id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO job_effects (job_id, seq, kind, target, summary, dry_run)
             VALUES (?1, 0, 'message', '+15555550100', 'quoted the Hendersons fence job at $1,240', 0)",
            [id],
        )
        .unwrap();
        id
    };

    // `forge job list --json` carries the job, and narrows by project.
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["id"], job_id);
    assert_eq!(rows[0]["project"], "equitizr");
    assert_eq!(rows[0]["workflow"], "quote-by-text");
    assert_eq!(rows[0]["trigger_kind"], "manual");
    assert_eq!(rows[0]["state"], "ok");
    assert_eq!(rows[0]["dry_run"], false);
    assert_eq!(rows[0]["cost_usd"], 0.02);

    let scoped: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "list", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(scoped.as_array().unwrap().len(), 1);
    assert!(
        !e.forge("ok.sh", &["job", "list", "no-such-project", "--json"])
            .status
            .success()
    );

    // `forge job show <id> --json` carries the job with its step and effect.
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &job_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["id"], job_id);
    assert_eq!(doc["workflow_hash"], "deadbeef");
    assert_eq!(doc["landed_sha"], "cafef00d");
    let steps = doc["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0]["action"], "extract-job");
    assert_eq!(steps[0]["kind"], "directive");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0]["kind"], "message");
    assert_eq!(effects[0]["target"], "+15555550100");

    // A job id that doesn't exist is refused.
    assert!(
        !e.forge("ok.sh", &["job", "show", "999", "--json"])
            .status
            .success()
    );

    // `forge job log <project> --json` carries the same effect, newest first.
    let log: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "log", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    let log = log.as_array().unwrap();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0]["job_id"], job_id);
    assert_eq!(
        log[0]["summary"],
        "quoted the Hendersons fence job at $1,240"
    );
}

/// The executor (src/job.rs): a fixture run workflow of two built-in
/// effect operations (`write-file`, `append-row`) and one assertion that
/// reads the effect log. `forge job start --now` runs both operations for
/// real, in a scratch directory archived from the project's repository,
/// and records two effects; `--dry-run` records the same two effects
/// marked dry, and neither operation actually writes its file (see
/// docs/JOBS.md, "The executor").
#[test]
fn forge_job_start_now_runs_operations_inline_and_dry_run_writes_nothing() {
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

    // Ensure the operator's workflow catalog exists, which is also where
    // the built-in effect operations (write-file, append-row, ...) get
    // written the first time anything loads it.
    assert!(e.forge("ok.sh", &["workflows"]).status.success());

    std::fs::write(
        e.home.join("workflows/snapshot.toml"),
        r#"name = "snapshot"
kind = "run"
description = "writes a file and appends a row: the two operations the executor runs inline"

steps = [
  { action = "write-file", effect = "file" },
  { action = "append-row", effect = "row" },
]

[trigger]
on = "manual"

[assert]
effects = ["bash", "-c", "grep -q '^file' \"$FORGE_EFFECT_LOG\" && grep -q '^row' \"$FORGE_EFFECT_LOG\""]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
    )
    .unwrap();

    let input = e.home.join("input.json");
    std::fs::write(
        &input,
        r#"{"path":"out.txt","content":"hello world","table":"book.csv","row":"hello,42"}"#,
    )
    .unwrap();
    let input_s = input.to_str().unwrap();

    // --now: the steps run for real, both effects land, and the files
    // they claim to have written are really there, in the job's own
    // scratch directory.
    let o = e.forge(
        "ok.sh",
        &[
            "job", "start", "equitizr", "snapshot", "--input", input_s, "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    assert_eq!(doc["dry_run"], false);
    assert_eq!(doc["cost_usd"], 0.0);
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert_eq!(effects[0]["kind"], "file");
    assert_eq!(effects[0]["target"], "out.txt");
    assert!(!effects[0]["dry_run"].as_bool().unwrap());
    assert_eq!(effects[1]["kind"], "row");
    assert_eq!(effects[1]["target"], "book.csv");
    assert!(!effects[1]["dry_run"].as_bool().unwrap());

    let scratch = e.home.join("worktrees").join(format!("job-{id}"));
    assert_eq!(
        std::fs::read_to_string(scratch.join("out.txt")).unwrap(),
        "hello world"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("book.csv")).unwrap(),
        "hello,42\n"
    );

    // --dry-run --now: the same effects are recorded, marked dry, but
    // neither file is actually written.
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "snapshot",
            "--input",
            input_s,
            "--dry-run",
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let dry_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    assert_ne!(dry_id, id);

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &dry_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    assert_eq!(doc["dry_run"], true);
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert!(effects.iter().all(|x| x["dry_run"].as_bool().unwrap()));
    assert!(
        effects[0]["summary"].as_str().unwrap().contains("dry run"),
        "{effects:?}"
    );
    assert!(
        effects[1]["summary"].as_str().unwrap().contains("dry run"),
        "{effects:?}"
    );

    let dry_scratch = e.home.join("worktrees").join(format!("job-{dry_id}"));
    assert!(!dry_scratch.join("out.txt").exists());
    assert!(!dry_scratch.join("book.csv").exists());
}

/// A run workflow with a directive step is refused with a clear message:
/// directive steps are the next build-order step (docs/JOBS.md), not this
/// one.
#[test]
fn forge_job_start_refuses_a_directive_step() {
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
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/with-directive.toml"),
        r#"name = "with-directive"
kind = "run"
description = "a job step names a directive, which this executor refuses"

steps = [
  { action = "code", role = "write" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
    )
    .unwrap();

    let o = e.forge(
        "ok.sh",
        &["job", "start", "equitizr", "with-directive", "--now"],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("directive"), "{err}");
}
