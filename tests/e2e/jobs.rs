use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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

/// `forge job start --now` emits `JobStarted` then `JobFinished` (see
/// docs/JOBS.md, "The executor"): `forge events` shows both, carrying the
/// job's id and its final state, so a client's jobs list knows when to
/// re-read without polling (docs/REVIEW-2.md item 7).
#[test]
fn forge_job_start_now_emits_job_started_and_job_finished_events() {
    let e = Env::new();
    setup_snapshot_workflow(&e);

    let input = e.home.join("input.json");
    std::fs::write(
        &input,
        r#"{"path":"out.txt","content":"hello world","table":"book.csv","row":"hello,42"}"#,
    )
    .unwrap();
    let input_s = input.to_str().unwrap();

    let o = e.forge(
        "ok.sh",
        &[
            "job", "start", "equitizr", "snapshot", "--input", input_s, "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let events = e.forge("ok.sh", &["events"]);
    assert!(events.status.success());
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&events.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let started_idx = lines
        .iter()
        .position(|v| v["type"] == "job_started" && v["job_id"] == id)
        .unwrap_or_else(|| panic!("no job_started event for job {id} in:\n{lines:#?}"));
    let finished_idx = lines
        .iter()
        .position(|v| v["type"] == "job_finished" && v["job_id"] == id)
        .unwrap_or_else(|| panic!("no job_finished event for job {id} in:\n{lines:#?}"));
    assert!(
        started_idx < finished_idx,
        "job_started should precede job_finished: {lines:#?}"
    );
    assert_eq!(lines[started_idx]["project"], "equitizr");
    assert_eq!(lines[started_idx]["workflow"], "snapshot");
    assert_eq!(lines[started_idx]["dry_run"], false);
    assert_eq!(lines[finished_idx]["state"], "ok");
}

/// `forge job start` without `--now` only queues the job; the worker's
/// claim loop (`src/worker.rs`) claims it alongside tasks, within the
/// same `--jobs` cap, and runs it through `src/job.rs` exactly as `--now`
/// would have (docs/JOBS.md step 1d). `forge work --once` drains it to
/// `ok` with no agent and no task in the mix.
#[test]
fn a_queued_job_is_claimed_and_run_by_forge_work_once() {
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

    // No `--now`: the job is only recorded, queued.
    let o = e.forge(
        "ok.sh",
        &["job", "start", "equitizr", "snapshot", "--input", input_s],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "queued", "{doc:?}");
    assert!(doc["effects"].as_array().unwrap().is_empty());

    // The worker claims it and runs it to completion; no task is in the
    // queue, so this is the job alone driving `forge work --once`.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    assert_eq!(doc["dry_run"], false);
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert_eq!(effects[0]["kind"], "file");
    assert_eq!(effects[1]["kind"], "row");

    let scratch = e.home.join("worktrees").join(format!("job-{id}"));
    assert_eq!(
        std::fs::read_to_string(scratch.join("out.txt")).unwrap(),
        "hello world"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("book.csv")).unwrap(),
        "hello,42\n"
    );

    // The project rollup counts the job separately from its (empty) task
    // counts.
    let project: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "show", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(project["jobs_today"], 1, "{project:?}");
    assert_eq!(project["jobs_ok"], 1, "{project:?}");
    assert_eq!(project["jobs_failed"], 0, "{project:?}");
    assert_eq!(project["jobs_needs_human"], 0, "{project:?}");
}

/// A directive job step needs `role`, to route its provider, and its
/// action needs `schema`, to hold its structured output to (docs/JOBS.md,
/// "Steps"). The built-in `code` directive has neither role nor schema, so
/// it names both gaps in turn.
#[test]
fn a_directive_job_step_needs_a_role_and_the_actions_schema() {
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
        e.home.join("workflows/no-role.toml"),
        r#"name = "no-role"
kind = "run"
description = "a directive job step names no role"

steps = [
  { action = "code" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
    )
    .unwrap();
    let o = e.forge("ok.sh", &["job", "start", "equitizr", "no-role", "--now"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("role"), "{err}");

    std::fs::write(
        e.home.join("workflows/no-schema.toml"),
        r#"name = "no-schema"
kind = "run"
description = "a directive job step's action names no schema"

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
    let o = e.forge("ok.sh", &["job", "start", "equitizr", "no-schema", "--now"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("schema"), "{err}");
}

/// A directive job step's own action (docs/JOBS.md, "Steps"): an
/// `extract-job` directive with a `role` and a `schema`, feeding a
/// `log-price` operation that reads its validated output back
/// (`FORGE_OUTPUT_EXTRACT_JOB`). Written once and shared by the
/// schema-valid and schema-invalid tests below.
const EXTRACT_JOB_ACTION: &str = r#"name = "extract-job"
kind = "directive"
contract = "plan"
description = "extract a structured job description and a price hint from the customer's message"
prompt = "Return only the fields the schema names."
schema = '''
{"type":"object","additionalProperties":false,"required":["job","price_hint"],"properties":{"job":{"type":"string"},"price_hint":{"type":"number"}}}
'''
"#;

const LOG_PRICE_ACTION: &str = r#"name = "log-price"
kind = "operation"
description = "read extract-job's validated output and log a row effect naming the price"
run = ["bash", "-c", "price=$(sed -n 's/.*\"price_hint\":\\([0-9.]*\\).*/\\1/p' \"$FORGE_OUTPUT_EXTRACT_JOB\"); printf 'row\\tbook.csv\\tpriced at %s\\n' \"$price\" >> \"$FORGE_EFFECT_LOG\""]
"#;

const QUOTE_WORKFLOW: &str = r#"name = "quote"
kind = "run"
description = "extract a job then log its price: a directive feeding an operation"

steps = [
  { action = "extract-job", role = "read" },
  { action = "log-price",   effect = "row" },
]

[trigger]
on = "manual"

[assert]
priced = ["bash", "-c", "grep -q '^row' \"$FORGE_EFFECT_LOG\""]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#;

fn setup_quote_workflow(e: &Env) {
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
        e.home.join("workflows/actions/extract-job.toml"),
        EXTRACT_JOB_ACTION,
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/actions/log-price.toml"),
        LOG_PRICE_ACTION,
    )
    .unwrap();
    std::fs::write(e.home.join("workflows/quote.toml"), QUOTE_WORKFLOW).unwrap();
}

/// A directive step's schema-valid structured output is written as its
/// output, recorded with its provider and real cost, and flows to the
/// operation after it (docs/JOBS.md, "Steps" and "The executor").
#[test]
fn a_directives_schema_valid_output_flows_to_the_next_operation() {
    let e = Env::new();
    setup_quote_workflow(&e);

    let mut c = e.with_role("ok.sh", "EXTRACT_JOB", "job-directive-valid.sh");
    let o = c
        .args(["job", "start", "equitizr", "quote", "--now"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    assert!(doc["cost_usd"].as_f64().unwrap() > 0.0, "{doc:?}");
    let steps = doc["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(steps[0]["action"], "extract-job");
    assert_eq!(steps[0]["kind"], "directive");
    assert_eq!(steps[0]["provider"], "anthropic");
    assert!(!steps[0]["model"].as_str().unwrap().is_empty(), "{steps:?}");
    assert!(steps[0]["cost_usd"].as_f64().unwrap() > 0.0, "{steps:?}");
    assert!(!steps[0]["output_ref"].as_str().unwrap().is_empty());
    assert_eq!(steps[1]["action"], "log-price");
    assert_eq!(steps[1]["kind"], "operation");

    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0]["kind"], "row");
    assert!(
        effects[0]["summary"].as_str().unwrap().contains("250"),
        "the price the directive extracted did not reach the operation: {effects:?}"
    );
}

/// The per-run budget in `[limits]` is enforced after every directive: a
/// step whose cost brings the run over it ends the job `needs_human`
/// rather than `ok` or `failed`, and the operation after it never runs
/// (docs/JOBS.md, "Steps").
#[test]
fn a_directive_step_over_budget_ends_the_job_needs_human() {
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
        e.home.join("workflows/actions/extract-job.toml"),
        EXTRACT_JOB_ACTION,
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/actions/log-price.toml"),
        LOG_PRICE_ACTION,
    )
    .unwrap();
    std::fs::write(
        e.home.join("workflows/tight-budget.toml"),
        r#"name = "tight-budget"
kind = "run"
description = "a per-run budget the one directive step already exceeds"

steps = [
  { action = "extract-job", role = "read" },
  { action = "log-price",   effect = "row" },
]

[trigger]
on = "manual"

[assert]
priced = ["bash", "-c", "grep -q '^row' \"$FORGE_EFFECT_LOG\""]

[limits]
budget_usd = 0.0001
per_day = 10
on_failure = "drop"
"#,
    )
    .unwrap();

    let mut c = e.with_role("ok.sh", "EXTRACT_JOB", "job-directive-valid.sh");
    let o = c
        .args(["job", "start", "equitizr", "tight-budget", "--now"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "needs_human", "{doc:?}");
    let steps = doc["steps"].as_array().unwrap();
    assert_eq!(
        steps.len(),
        1,
        "the operation must not run once the budget is exceeded: {steps:?}"
    );
    assert!(
        doc["verdict_json"].as_str().unwrap().contains("budget"),
        "{doc:?}"
    );
    assert!(doc["effects"].as_array().unwrap().is_empty());
}

/// A directive step's structured output that does not match its action's
/// schema fails the job with the validation message, before the operation
/// after it ever runs (docs/JOBS.md, "Steps").
#[test]
fn a_directives_schema_invalid_output_fails_the_job_with_the_validation_message() {
    let e = Env::new();
    setup_quote_workflow(&e);

    let mut c = e.with_role("ok.sh", "EXTRACT_JOB", "job-directive-invalid.sh");
    let o = c
        .args(["job", "start", "equitizr", "quote", "--now"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "failed", "{doc:?}");
    let steps = doc["steps"].as_array().unwrap();
    assert_eq!(
        steps.len(),
        1,
        "the operation after the directive must not run: {steps:?}"
    );
    assert_eq!(steps[0]["action"], "extract-job");
    assert!(
        doc["verdict_json"]
            .as_str()
            .unwrap()
            .contains("does not match the schema"),
        "{doc:?}"
    );
    assert!(
        doc["effects"].as_array().unwrap().is_empty(),
        "log-price never ran: {doc:?}"
    );
}

/// A directive step whose agent run itself fails (docs/JOBS.md, "Steps"): no
/// structured output, a `result` frame carrying an error subtype, and a line
/// on stderr — what a job step run through `run_directive` leaves behind is
/// like an attempt's own record: an event stream and stderr under
/// `FORGE2_HOME/logs/job-<id>-<seq>.jsonl`, the prompt as its first line,
/// the text the agent did return (there is no structured output to prefer)
/// as the step's `output_ref`, and a verdict tail that quotes the result's
/// own subtype and the stderr tail rather than a bare exit code. `forge job
/// show` prints that tail and names the log file.
#[test]
fn a_directives_failed_agent_run_leaves_a_log_an_output_and_a_tail_naming_the_subtype() {
    let e = Env::new();
    setup_quote_workflow(&e);

    let mut c = e.with_role("ok.sh", "EXTRACT_JOB", "job-directive-error.sh");
    let o = c
        .args(["job", "start", "equitizr", "quote", "--now"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "failed", "{doc:?}");
    let steps = doc["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 1, "the operation must not run: {steps:?}");
    assert_eq!(steps[0]["action"], "extract-job");
    assert!(
        !steps[0]["output_ref"].as_str().unwrap().is_empty(),
        "the plain text the agent returned instead of a structured result is \
         still kept as the step's output: {steps:?}"
    );
    let output_text = std::fs::read_to_string(steps[0]["output_ref"].as_str().unwrap()).unwrap();
    assert_eq!(output_text, "I could not complete this.");

    let verdict = doc["verdict_json"].as_str().unwrap();
    assert!(
        verdict.contains("error_during_execution"),
        "the tail quotes the result's own subtype: {verdict}"
    );
    assert!(
        verdict.contains("boom: something in the sandbox broke"),
        "the tail quotes the stderr tail: {verdict}"
    );
    assert!(
        !verdict.contains("agent exit"),
        "a directive's failure tail is never the bare exit code: {verdict}"
    );

    let log_path = e.home.join(format!("logs/job-{id}-0.jsonl"));
    assert!(
        log_path.is_file(),
        "the step's event stream and stderr are logged like an attempt's: {}",
        log_path.display()
    );
    let log = std::fs::read_to_string(&log_path).unwrap();
    let first_line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(
        first_line["type"], "forge_prompt",
        "the prompt comes first: {log}"
    );
    assert!(
        log.contains("boom: something in the sandbox broke"),
        "stderr is folded into the log too: {log}"
    );

    let show = e.forge("ok.sh", &["job", "show", &id.to_string()]);
    let text = String::from_utf8_lossy(&show.stdout);
    assert!(
        text.contains("error_during_execution") && text.contains("boom"),
        "forge job show prints the failed step's tail: {text}"
    );
    assert!(
        text.contains(log_path.to_str().unwrap()),
        "forge job show names the log file: {text}"
    );

    assert!(
        doc["effects"].as_array().unwrap().is_empty(),
        "log-price never ran: {doc:?}"
    );
}

/// `forge job start` resolves a run workflow from the project's own
/// repository first (docs/JOBS.md, "Where an automation lives"): a fixture
/// repository with `.forge/workflows/publish-snapshot.toml` landed on its
/// base branch, no matching file anywhere in the operator's catalog. The
/// job runs the same way a catalog-sourced one does, and its record says
/// the repository was the source.
#[test]
fn forge_job_start_resolves_a_run_workflow_from_the_projects_repository_and_records_the_source() {
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

    // The operator's catalog exists, and has never heard of this workflow.
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    assert!(!e.home.join("workflows/publish-snapshot.toml").exists());

    // The automation lives in the project's own repository, committed on
    // its base branch — not written into FORGE2_HOME/workflows.
    std::fs::create_dir_all(e.repo.join(".forge/workflows")).unwrap();
    std::fs::write(
        e.repo.join(".forge/workflows/publish-snapshot.toml"),
        r#"name = "publish-snapshot"
kind = "run"
description = "publishes equitizr's snapshot: a built-in effect operation, no project actions needed"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[assert]
published = ["bash", "-c", "grep -q '^file' \"$FORGE_EFFECT_LOG\""]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &["commit", "-qm", "add publish-snapshot automation"],
    );

    // `forge workflows --project equitizr` lists it beside the operator's
    // catalog, without it ever having landed in FORGE2_HOME/workflows.
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["workflows", "--project", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    let listed = doc["workflows"].as_array().unwrap();
    let repo_wf = listed
        .iter()
        .find(|w| w["name"] == "publish-snapshot")
        .unwrap_or_else(|| panic!("publish-snapshot missing from --project listing: {listed:?}"));
    assert_eq!(repo_wf["source"], "repo");
    assert!(
        !listed
            .iter()
            .any(|w| w["name"] == "publish-snapshot" && w["source"] == "catalog"),
        "the operator's own catalog never gained a copy: {listed:?}"
    );

    let input = e.home.join("input.json");
    std::fs::write(&input, r#"{"path":"snapshot.txt","content":"snapshot"}"#).unwrap();
    let input_s = input.to_str().unwrap();

    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "publish-snapshot",
            "--input",
            input_s,
            "--now",
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
    assert_eq!(
        doc["workflow_source"], "repo",
        "the job's record says the workflow came from the project's own repository: {doc:?}"
    );
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0]["kind"], "file");
    assert_eq!(effects[0]["target"], "snapshot.txt");

    let scratch = e.home.join("worktrees").join(format!("job-{id}"));
    assert_eq!(
        std::fs::read_to_string(scratch.join("snapshot.txt")).unwrap(),
        "snapshot"
    );

    // A name the repository does not carry at all still falls to the
    // operator's catalog, unchanged.
    let o = e.forge(
        "ok.sh",
        &["job", "start", "equitizr", "no-such-workflow", "--now"],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("unknown workflow"), "{err}");
}

/// `forge job bench`: the repository's own `.forge/workflows/changelog-line.toml`
/// and two of its four real fixtures (`.forge/fixtures/changelog-line/`),
/// run once per provider in dry-run mode. Two fake providers, told apart
/// by `JOB_BENCH_PROVIDER` (set through each one's own `[providers.<name>].env`,
/// since the agent binary is chosen by step name alone — see
/// `tests/fakes/job-bench.sh`): "anthropic" classifies both fixtures
/// right, "devhome" is free and mislabels the fix as a chore. `bench`
/// measures exactly that gap (docs/JOBS.md, "Steps": "the bounded
/// judgment the local model is fit for").
#[test]
fn forge_job_bench_measures_two_fake_providers_over_two_fixtures() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[providers.anthropic]\n\
         runner = \"claude-cli\"\n\
         env = { JOB_BENCH_PROVIDER = \"anthropic\" }\n\
         [providers.devhome]\n\
         runner = \"claude-cli\"\n\
         env = { JOB_BENCH_PROVIDER = \"devhome\" }\n",
    )
    .unwrap();

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

    // The real automation this task ships, copied into the project's own
    // repository the way docs/JOBS.md says an automation lives — not
    // rewritten for the test, so the test exercises exactly the files
    // that are checked in.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let wf_dir = e.repo.join(".forge/workflows");
    std::fs::create_dir_all(wf_dir.join("actions")).unwrap();
    std::fs::copy(
        root.join(".forge/workflows/changelog-line.toml"),
        wf_dir.join("changelog-line.toml"),
    )
    .unwrap();
    for f in [
        "summarise-changelog-line.toml",
        "append-changelog-line.toml",
    ] {
        std::fs::copy(
            root.join(".forge/workflows/actions").join(f),
            wf_dir.join("actions").join(f),
        )
        .unwrap();
    }
    let fx_dir = e.repo.join(".forge/fixtures/changelog-line");
    std::fs::create_dir_all(&fx_dir).unwrap();
    for f in ["01-fix.json", "03-docs.json"] {
        std::fs::copy(
            root.join(".forge/fixtures/changelog-line").join(f),
            fx_dir.join(f),
        )
        .unwrap();
    }

    let o = e
        .cmd("job-bench.sh")
        .args([
            "job",
            "bench",
            "equitizr",
            "changelog-line",
            "--providers",
            "anthropic,devhome",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8_lossy(&o.stdout);
    eprintln!("{stdout}");
    assert!(stdout.contains("anthropic"), "{stdout}");
    assert!(stdout.contains("devhome"), "{stdout}");
    assert!(
        stdout.matches("2/2 (100%)").count() >= 3,
        "both providers: 2/2 schema-valid, and anthropic also 2/2 expected-kind: {stdout}"
    );
    assert!(
        stdout.contains("1/2 (50%)"),
        "devhome mislabels the fix as a chore: {stdout}"
    );

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "list", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 4, "two providers over two fixtures: {rows:?}");
    assert!(
        rows.iter().all(|r| r["dry_run"] == true),
        "bench never performs a real effect: {rows:?}"
    );
}

/// The engineering-weekly job (scheduler task 7 of 7; docs/JOBS.md, "Where
/// an automation lives"): the real workflow and its three scripts, copied
/// unmodified into the project's own repository except for
/// `SRC_FILE_MAX_LINES`, lowered from 3000 to 5 so a small fixture file
/// crosses it deterministically and fast, with no Cargo.toml in the
/// fixture to make measure.sh actually build or lint anything. A dry run
/// still measures — the first `row` effect carries the JSON document — but
/// files nothing: the second `row` effect says it would have, marked
/// "(dry run)", and `forge log` afterward still shows no task on the
/// project, proving `forge add` itself was never called.
#[test]
fn engineering_weekly_dry_run_measures_and_files_nothing() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "acme", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let wf_dir = e.repo.join(".forge/workflows");
    std::fs::create_dir_all(wf_dir.join("actions")).unwrap();

    let wf_text =
        std::fs::read_to_string(root.join(".forge/workflows/engineering-weekly.toml")).unwrap();
    let wf_text = wf_text.replace(
        "SRC_FILE_MAX_LINES = \"3000\"",
        "SRC_FILE_MAX_LINES = \"5\"",
    );
    assert!(
        wf_text.contains("SRC_FILE_MAX_LINES = \"5\""),
        "the real workflow's threshold line changed shape; update this test's replace"
    );
    std::fs::write(wf_dir.join("engineering-weekly.toml"), wf_text).unwrap();

    for f in ["measure-engineering.toml", "review-if-crossed.toml"] {
        std::fs::copy(
            root.join(".forge/workflows/actions").join(f),
            wf_dir.join("actions").join(f),
        )
        .unwrap();
    }
    for f in ["measure.sh", "compare-thresholds.sh", "skip-if-reviewed.sh"] {
        std::fs::copy(
            root.join(".forge/workflows/actions").join(f),
            wf_dir.join("actions").join(f),
        )
        .unwrap();
    }

    // A fixture file over the test's lowered threshold, nowhere near a
    // real 3000-line one, and no Cargo.toml: measure.sh's cargo
    // test/clippy block never runs.
    std::fs::create_dir_all(e.repo.join("src")).unwrap();
    std::fs::write(
        e.repo.join("src/big.rs"),
        "// a fixture line, over the lowered threshold\n".repeat(10),
    )
    .unwrap();

    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "engineering-weekly fixture"]);

    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "acme",
            "engineering-weekly",
            "--dry-run",
            "--now",
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
    assert_eq!(doc["dry_run"], true);
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");

    assert_eq!(effects[0]["kind"], "row");
    assert_eq!(effects[0]["target"], "measurements.json");
    let measured = effects[0]["summary"].as_str().unwrap();
    assert!(measured.contains("kernel_lines"), "{measured}");
    assert!(measured.contains("clippy_warnings"), "{measured}");

    assert_eq!(effects[1]["kind"], "row");
    assert_eq!(effects[1]["target"], "task");
    let filed = effects[1]["summary"].as_str().unwrap();
    assert!(filed.contains("docs/REVIEW"), "{filed}");
    assert!(filed.contains("over 5 lines"), "{filed}");
    assert!(filed.contains("(dry run)"), "{filed}");

    // Nothing was actually filed: `forge add` was never called.
    let tasks: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["log", "--project", "acme", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 0, "{tasks:?}");

    // A real run (the dry run above never counted against `per_day`)
    // really does call `forge add`: the crossed threshold lands as a
    // queued task whose text starts with "docs/REVIEW".
    let o = e.forge(
        "ok.sh",
        &["job", "start", "acme", "engineering-weekly", "--now"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let tasks: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["log", "--project", "acme", "--json"])
            .stdout,
    )
    .unwrap();
    let tasks = tasks.as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0]["state"], "queued");
    assert!(
        tasks[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("docs/REVIEW"),
        "{tasks:?}"
    );
}

/// `Limits.per_day` is checked at `forge job start`: once a workflow has
/// started that many real runs in the last 24 hours, the next start is
/// refused with a reason naming the limit, queued or `--now` alike; a
/// dry run neither counts nor is refused (docs/JOBS.md, "Limits").
#[test]
fn a_run_workflows_per_day_limit_refuses_the_next_start_and_ignores_dry_runs() {
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
        e.home.join("workflows/once.toml"),
        r#"name = "once"
kind = "run"
description = "one real run a day"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[limits]
budget_usd = 1.0
per_day = 1
on_failure = "drop"
"#,
    )
    .unwrap();
    let input = e.home.join("input.json");
    std::fs::write(&input, r#"{"path":"out.txt","content":"x"}"#).unwrap();
    let input_s = input.to_str().unwrap();
    let start = |extra: &[&str]| {
        let mut args = vec!["job", "start", "equitizr", "once", "--input", input_s];
        args.extend_from_slice(extra);
        e.forge("ok.sh", &args)
    };

    // A dry run first: it does not count.
    let o = start(&["--now", "--dry-run"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // The first real run starts.
    let o = start(&["--now"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // The second is refused, naming the limit; queuing is refused the same way.
    let o = start(&["--now"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("per_day limit is 1"), "{err}");
    let o = start(&[]);
    assert!(!o.status.success(), "queuing past the limit is refused too");
    // A dry run is still allowed.
    let o = start(&["--now", "--dry-run"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// The worker's schedule trigger (docs/JOBS.md, "Triggers"): a run
/// workflow committed to a project's own repository with `[trigger] on =
/// "schedule"` and a `cron` that matches every minute fires on its own —
/// `forge work --once` starts it with no `forge job start` at all, exactly
/// once, recorded with `trigger_kind = "schedule"` and `trigger_ref` the
/// slot's unix second. Running the worker again right away, still inside
/// the same minute, does not start a second job for it.
#[test]
fn a_schedule_trigger_starts_exactly_one_job_via_forge_work_once() {
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

    std::fs::create_dir_all(e.repo.join(".forge/workflows/actions")).unwrap();
    std::fs::write(
        e.repo.join(".forge/workflows/actions/noop.toml"),
        "name = \"noop\"\nkind = \"operation\"\ndescription = \"always succeeds; nothing to run\"\nrun = [\"true\"]\n",
    )
    .unwrap();
    std::fs::write(
        e.repo.join(".forge/workflows/tick.toml"),
        r#"name = "tick"
kind = "run"
description = "fires every minute, for e2e coverage of the schedule trigger"

steps = [
  { action = "noop" },
]

[trigger]
on = "schedule"
cron = "* * * * *"

[assert]
ok = ["true"]
"#,
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "add the tick automation"]);

    assert!(
        e.forge("ok.sh", &["job", "list", "--json"])
            .stdout
            .starts_with(b"[]")
    );

    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["project"], "equitizr");
    assert_eq!(rows[0]["workflow"], "tick");
    assert_eq!(rows[0]["trigger_kind"], "schedule");
    let trigger_ref = rows[0]["trigger_ref"].as_str().unwrap();
    let slot: i64 = trigger_ref.parse().unwrap();
    assert_eq!(slot % 60, 0, "the slot is a minute boundary: {trigger_ref}");

    // Running the worker again right away, still inside the same minute,
    // finds nothing new due: the same slot never starts a second job.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows:?}");
}

fn setup_snapshot_workflow(e: &Env) {
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
}

/// Delayed jobs (docs/JOBS.md, "Delayed jobs"): `forge job start --delay
/// 1h` leaves the job `scheduled`, carrying its `due_at`, visible in
/// `forge job list`; `forge work --once` finds nothing claimable and
/// leaves it untouched. `--delay 0s` is due immediately, so the very same
/// `--once` pass claims and runs it — the wait is `due_at` on the row,
/// never an in-memory timer, so it is exactly what a restart would see too.
#[test]
fn forge_job_start_delay_leaves_the_job_scheduled_until_due_and_zero_runs_it() {
    let e = Env::new();
    setup_snapshot_workflow(&e);

    let input = e.home.join("input.json");
    std::fs::write(
        &input,
        r#"{"path":"out.txt","content":"hello world","table":"book.csv","row":"hello,42"}"#,
    )
    .unwrap();
    let input_s = input.to_str().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // `--now` and `--delay` are refused together: one runs inline
    // immediately, the other leaves the job waiting.
    let o = e.forge(
        "ok.sh",
        &[
            "job", "start", "equitizr", "snapshot", "--input", input_s, "--now", "--delay", "1h",
        ],
    );
    assert!(!o.status.success());

    // --delay 1h: scheduled, due about an hour from now.
    let o = e.forge(
        "ok.sh",
        &[
            "job", "start", "equitizr", "snapshot", "--input", input_s, "--delay", "1h",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "scheduled", "{doc:?}");
    let due_at = doc["due_at"].as_i64().unwrap();
    assert!(
        (now + 3500..=now + 3700).contains(&due_at),
        "due_at {due_at} is not about an hour from now ({now})"
    );

    // `forge job show` names the due time in plain text too.
    let show = e.forge("ok.sh", &["job", "show", &id.to_string()]);
    let text = String::from_utf8_lossy(&show.stdout);
    assert!(text.contains(&due_at.to_string()), "{text}");

    // `forge job list` shows it, scheduled, with when it is due, in both
    // the machine-readable and the plain forms.
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(row["state"], "scheduled");
    assert_eq!(row["due_at"], due_at);

    let list = e.forge("ok.sh", &["job", "list"]);
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        text.contains("scheduled") && text.contains(&due_at.to_string()),
        "{text}"
    );

    // `forge work --once` finds nothing claimable: the job is left exactly
    // where it was.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "scheduled", "still waiting: {doc:?}");
    assert!(doc["effects"].as_array().unwrap().is_empty());

    // --delay 0s: already due, so it is queued (not scheduled) right away.
    let o = e.forge(
        "ok.sh",
        &[
            "job", "start", "equitizr", "snapshot", "--input", input_s, "--delay", "0s",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let zero_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &zero_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "queued", "due now, not scheduled: {doc:?}");

    // The very same --once pass claims and runs it.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &zero_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");

    // The hour-delayed job is still untouched by any of this.
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "scheduled", "{doc:?}");
}

/// `forge job withdraw` (docs/JOBS.md, "Delayed jobs"): a scheduled job is
/// dropped before it ever becomes due. Withdrawing it again, or a job that
/// was never scheduled (queued, or already run), is refused.
#[test]
fn forge_job_withdraw_drops_a_scheduled_job_and_refuses_any_other_state() {
    let e = Env::new();
    setup_snapshot_workflow(&e);

    let input = e.home.join("input.json");
    std::fs::write(
        &input,
        r#"{"path":"out.txt","content":"hello world","table":"book.csv","row":"hello,42"}"#,
    )
    .unwrap();
    let input_s = input.to_str().unwrap();

    let o = e.forge(
        "ok.sh",
        &[
            "job", "start", "equitizr", "snapshot", "--input", input_s, "--delay", "1h",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let scheduled_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    // A plain queued job (never scheduled) cannot be withdrawn.
    let o = e.forge(
        "ok.sh",
        &["job", "start", "equitizr", "snapshot", "--input", input_s],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let queued_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    let o = e.forge("ok.sh", &["job", "withdraw", &queued_id.to_string()]);
    assert!(!o.status.success());

    // The scheduled one withdraws cleanly.
    let o = e.forge("ok.sh", &["job", "withdraw", &scheduled_id.to_string()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge(
            "ok.sh",
            &["job", "show", &scheduled_id.to_string(), "--json"],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "dropped", "{doc:?}");

    // Withdrawing it again is refused.
    let o = e.forge("ok.sh", &["job", "withdraw", &scheduled_id.to_string()]);
    assert!(!o.status.success());

    // `forge work --once` never touches a dropped job.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge(
            "ok.sh",
            &["job", "show", &scheduled_id.to_string(), "--json"],
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "dropped", "{doc:?}");
}

/// `[skip_if]` (docs/JOBS.md, "Skipping a run"): a named command, run in
/// the scratch tree with the job's environment before any step. Exit 0
/// ends the job `Skipped` with its stdout's first line as the reason,
/// before any step runs and so before any effect happens or the
/// `[assert]` commands are even reached; exit 1 means "not skipped,
/// proceed" and the steps run exactly as they would with no `[skip_if]`
/// at all. `forge job list`/`show` and the portal (`forge project view`)
/// carry the skipped run as an ordinary job row.
#[test]
fn a_skip_if_that_exits_0_skips_the_job_and_one_that_exits_1_lets_the_steps_run() {
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
        e.home.join("workflows/skip-snapshot.toml"),
        r#"name = "skip-snapshot"
kind = "run"
description = "writes a file, unless skip_if says it is already handled"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[skip_if]
already_handled = ["bash", "-c", "if [ \"$FORGE_INPUT_SKIP\" = \"yes\" ]; then echo 'already handled, nothing to do'; exit 0; else exit 1; fi"]

[assert]
wrote = ["bash", "-c", "grep -q '^file' \"$FORGE_EFFECT_LOG\""]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
    )
    .unwrap();

    // The skip_if command exits 0: the job ends Skipped before the write-file
    // step ever runs, so no effect is logged and nothing lands in the
    // scratch tree.
    let skip_input = e.home.join("skip-input.json");
    std::fs::write(
        &skip_input,
        r#"{"path":"out.txt","content":"hello world","skip":"yes"}"#,
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "skip-snapshot",
            "--input",
            skip_input.to_str().unwrap(),
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let skipped_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &skipped_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "skipped", "{doc:?}");
    assert_eq!(doc["cost_usd"], 0.0);
    assert!(doc["effects"].as_array().unwrap().is_empty(), "{doc:?}");
    assert!(doc["steps"].as_array().unwrap().is_empty(), "{doc:?}");
    let verdict: Vec<serde_json::Value> =
        serde_json::from_str(doc["verdict_json"].as_str().unwrap()).unwrap();
    assert_eq!(verdict.len(), 1, "{verdict:?}");
    assert_eq!(verdict[0]["name"], "already_handled");
    assert_eq!(verdict[0]["ok"], true);
    assert_eq!(verdict[0]["tail"], "already handled, nothing to do");

    let skip_scratch = e.home.join("worktrees").join(format!("job-{skipped_id}"));
    assert!(
        !skip_scratch.join("out.txt").exists(),
        "the write-file step never ran"
    );

    // `forge job list` carries the skipped run as an ordinary row.
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(
        rows.iter().find(|r| r["id"] == skipped_id).unwrap()["state"],
        "skipped"
    );

    // The portal shows it too, with the reason.
    let portal: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "view", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    let wf = portal["run_workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "skip-snapshot")
        .unwrap_or_else(|| panic!("no skip-snapshot entry in {portal:?}"));
    let job_run = &wf["jobs"][0];
    assert_eq!(job_run["state"], "skipped");
    assert_eq!(
        job_run["reason"].as_str().unwrap(),
        "already handled, nothing to do"
    );

    // Rollups count the skip separately: `today` includes it but neither
    // `ok` nor `failed` does.
    let project: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "show", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(project["jobs_today"], 1, "{project:?}");
    assert_eq!(project["jobs_ok"], 0, "{project:?}");
    assert_eq!(project["jobs_failed"], 0, "{project:?}");
    assert_eq!(project["jobs_skipped"], 1, "{project:?}");

    // The skip_if command exits 1: not skipped, the steps run exactly as
    // they would with no [skip_if] at all.
    let proceed_input = e.home.join("proceed-input.json");
    std::fs::write(
        &proceed_input,
        r#"{"path":"out.txt","content":"hello world","skip":"no"}"#,
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "skip-snapshot",
            "--input",
            proceed_input.to_str().unwrap(),
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let ok_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    assert_ne!(ok_id, skipped_id);

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &ok_id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0]["kind"], "file");
    assert_eq!(effects[0]["target"], "out.txt");

    let ok_scratch = e.home.join("worktrees").join(format!("job-{ok_id}"));
    assert_eq!(
        std::fs::read_to_string(ok_scratch.join("out.txt")).unwrap(),
        "hello world"
    );
}

/// A `Skipped` job counts against nothing (docs/JOBS.md, "Skipping a
/// run"): it does not consume the workflow's `per_day` budget, unlike a
/// real (non-dry) run that reaches any other terminal state.
#[test]
fn a_skipped_job_does_not_count_toward_the_per_day_limit() {
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
        e.home.join("workflows/skip-once.toml"),
        r#"name = "skip-once"
kind = "run"
description = "always skips; used to check skip does not count against per_day"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[skip_if]
always = ["bash", "-c", "echo 'nothing to do'; exit 0"]

[limits]
budget_usd = 1.0
per_day = 1
on_failure = "drop"
"#,
    )
    .unwrap();
    let input = e.home.join("input.json");
    std::fs::write(&input, r#"{"path":"out.txt","content":"x"}"#).unwrap();
    let input_s = input.to_str().unwrap();

    // Two real (non-dry) runs, each skipped: neither counts against the
    // per_day = 1 limit, so both succeed.
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "skip-once",
            "--input",
            input_s,
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let first: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &first.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "skipped", "{doc:?}");

    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "skip-once",
            "--input",
            input_s,
            "--now",
        ],
    );
    assert!(
        o.status.success(),
        "a skipped run must not consume the per_day budget: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let second: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    assert_ne!(first, second);
    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &second.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "skipped", "{doc:?}");
}

fn write_fake(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

/// docs/CHECKS.md's drift-weekly automation, copied verbatim from this
/// repository's own `.forge/workflows/` into a throwaway project repo and
/// committed, then run with `claude`, `npm`, `cargo` and `curl` replaced by
/// fakes on PATH — so the check is deterministic and makes no real network
/// call. `claude` and `npm` agree on the version, `cargo` has no
/// `cargo-audit` and refuses to install one, and `curl` serves a fresh
/// `equitizr.com/api/meta`: the clean-week scenario the job's own
/// `[assert]` calls green. A dry run of it logs no effect (well under the
/// four steps' one-effect-each ceiling) and `job show --json` parses.
#[test]
fn drift_weekly_dry_run_records_at_most_four_effects_and_parses() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "forge",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(e.repo.join(".forge/workflows/actions")).unwrap();
    std::fs::copy(
        root.join(".forge/workflows/drift-weekly.toml"),
        e.repo.join(".forge/workflows/drift-weekly.toml"),
    )
    .unwrap();
    for action in [
        "check-claude-cli-version",
        "check-model-drift",
        "check-cargo-audit",
        "check-equitizr-freshness",
    ] {
        std::fs::copy(
            root.join(format!(".forge/workflows/actions/{action}.toml")),
            e.repo
                .join(format!(".forge/workflows/actions/{action}.toml")),
        )
        .unwrap();
    }
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &["commit", "-qm", "add the drift-weekly automation"],
    );

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake(
        &fakebin.join("claude"),
        "#!/bin/bash\necho '1.0.0 (Claude Code)'\n",
    );
    write_fake(
        &fakebin.join("npm"),
        "#!/bin/bash\nif [ \"$1\" = view ]; then echo 1.0.0; exit 0; fi\nexit 1\n",
    );
    write_fake(
        &fakebin.join("cargo"),
        "#!/bin/bash\nif [ \"$1\" = install ]; then exit 1; fi\nexit 0\n",
    );
    write_fake(
        &fakebin.join("curl"),
        r#"#!/bin/bash
out=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-o" ]; then out="$a"; fi
  prev="$a"
done
now_ms=$(( $(date -u +%s) * 1000 ))
printf '{"meta":{"built_at":%s}}' "$now_ms" > "$out"
printf 200
"#,
    );
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args([
            "job",
            "start",
            "forge",
            "drift-weekly",
            "--now",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "ok", "{doc:?}");
    assert_eq!(doc["dry_run"], true);
    let effects = doc["effects"].as_array().unwrap();
    assert!(effects.len() <= 4, "{effects:?}");
    assert_eq!(effects.len(), 0, "a clean week logs no effect: {effects:?}");
}

/// Same automation, but with two attempt logs planted directly under
/// `FORGE2_HOME/logs` naming different ids for the same model family a
/// week apart (a `system`/`init` frame, the shape `check-model-drift`
/// actually reads — see the action's description): `check-model-drift`
/// must notice the alias moved and log exactly one `row` effect naming
/// both ids, proving the drift path (not just the clean-week path above)
/// can fire.
#[test]
fn drift_weekly_model_drift_fires_when_an_alias_moved() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "forge",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(e.repo.join(".forge/workflows/actions")).unwrap();
    std::fs::copy(
        root.join(".forge/workflows/drift-weekly.toml"),
        e.repo.join(".forge/workflows/drift-weekly.toml"),
    )
    .unwrap();
    for action in [
        "check-claude-cli-version",
        "check-model-drift",
        "check-cargo-audit",
        "check-equitizr-freshness",
    ] {
        std::fs::copy(
            root.join(format!(".forge/workflows/actions/{action}.toml")),
            e.repo
                .join(format!(".forge/workflows/actions/{action}.toml")),
        )
        .unwrap();
    }
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &["commit", "-qm", "add the drift-weekly automation"],
    );

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake(
        &fakebin.join("claude"),
        "#!/bin/bash\necho '1.0.0 (Claude Code)'\n",
    );
    write_fake(
        &fakebin.join("npm"),
        "#!/bin/bash\nif [ \"$1\" = view ]; then echo 1.0.0; exit 0; fi\nexit 1\n",
    );
    write_fake(
        &fakebin.join("cargo"),
        "#!/bin/bash\nif [ \"$1\" = install ]; then exit 1; fi\nexit 0\n",
    );
    write_fake(
        &fakebin.join("curl"),
        r#"#!/bin/bash
out=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-o" ]; then out="$a"; fi
  prev="$a"
done
now_ms=$(( $(date -u +%s) * 1000 ))
printf '{"meta":{"built_at":%s}}' "$now_ms" > "$out"
printf 200
"#,
    );
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    // Every operation step now gets FORGE2_HOME explicitly, set to the
    // real store the `forge job start` process itself resolved (e.home,
    // from `Env::cmd`'s own `FORGE2_HOME` — untouched by this command's
    // `HOME` override), so check-model-drift.toml reads `e.home/logs`,
    // not `$HOME/.local/share/forge2/logs`.
    let logs = e.home.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let a = logs.join("a.jsonl");
    let b = logs.join("b.jsonl");
    std::fs::write(
        &a,
        r#"{"type":"system","subtype":"init","session_id":"a","model":"claude-sonnet-5"}"#,
    )
    .unwrap();
    std::fs::write(
        &b,
        r#"{"type":"system","subtype":"init","session_id":"b","model":"claude-sonnet-4-5"}"#,
    )
    .unwrap();
    let now = std::time::SystemTime::now();
    let three_days_ago = now - std::time::Duration::from_secs(3 * 24 * 3600);
    let ten_days_ago = now - std::time::Duration::from_secs(10 * 24 * 3600);
    std::fs::File::open(&a)
        .unwrap()
        .set_modified(three_days_ago)
        .unwrap();
    std::fs::File::open(&b)
        .unwrap()
        .set_modified(ten_days_ago)
        .unwrap();

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args([
            "job",
            "start",
            "forge",
            "drift-weekly",
            "--now",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["dry_run"], true);
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(
        effects.len(),
        1,
        "exactly one alias (sonnet) moved: {effects:?}"
    );
    let row = &effects[0];
    assert_eq!(row["kind"], "row");
    let summary = row["summary"].as_str().unwrap();
    assert!(
        summary.contains("claude-sonnet-5") && summary.contains("claude-sonnet-4-5"),
        "{summary:?} should name both ids"
    );
}

/// docs/CHECKS.md's doctor-daily automation, copied verbatim from this
/// repository's own `.forge/workflows/` into a throwaway project repo and
/// committed: `doctor-daily.toml` splices in the schedule-free
/// `disk-and-logs.toml` (`{ workflow = "disk-and-logs" }`, `job_steps`'
/// new run-workflow composition), so a dry run resolves both files into
/// one flat two-step job — `doctor-json-to-effects` (from doctor-daily.toml
/// itself) then `disk-and-logs-check` (spliced in from disk-and-logs.toml)
/// — proving the splice actually flattened rather than merely parsing. The
/// nested `forge doctor --json` this job's first step shells out to only
/// inherits a whitelisted environment (agent::agent_env — PATH, HOME, ...),
/// so `claude` is faked on PATH the same way the drift-weekly test fakes
/// its externals, and `df`/`du` are faked so disk-and-logs-check's free
/// space and log size readings do not depend on the real machine's disk.
/// A fresh FORGE2_HOME always has at least one WARN (`rate_limit: no
/// samples yet`), so the effect log is never empty.
#[test]
fn doctor_daily_dry_run_parses_resolves_and_records_effects() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "forge",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(e.repo.join(".forge/workflows/actions")).unwrap();
    for workflow in ["doctor-daily", "disk-and-logs"] {
        std::fs::copy(
            root.join(format!(".forge/workflows/{workflow}.toml")),
            e.repo.join(format!(".forge/workflows/{workflow}.toml")),
        )
        .unwrap();
    }
    for action in ["doctor-json-to-effects", "disk-and-logs-check"] {
        std::fs::copy(
            root.join(format!(".forge/workflows/actions/{action}.toml")),
            e.repo
                .join(format!(".forge/workflows/actions/{action}.toml")),
        )
        .unwrap();
    }
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &["commit", "-qm", "add the doctor-daily automation"],
    );

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake(
        &fakebin.join("claude"),
        "#!/bin/bash\necho '1.0.0 (Claude Code)'\n",
    );
    write_fake(
        &fakebin.join("df"),
        "#!/bin/bash\necho Avail\necho 107374182400\n",
    );
    write_fake(
        &fakebin.join("du"),
        "#!/bin/bash\nprintf '4096\\t%s\\n' \"${@: -1}\"\n",
    );
    let forge_dir = Path::new(env!("CARGO_BIN_EXE_forge"))
        .parent()
        .unwrap()
        .to_path_buf();
    let path = format!(
        "{}:{}:{}",
        fakebin.display(),
        forge_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args([
            "job",
            "start",
            "forge",
            "doctor-daily",
            "--now",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["dry_run"], true);
    assert!(
        matches!(doc["state"].as_str(), Some("ok" | "failed" | "needs_human")),
        "{doc:?}"
    );
    let steps = doc["steps"].as_array().unwrap();
    assert_eq!(
        steps
            .iter()
            .map(|s| s["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["doctor-json-to-effects", "disk-and-logs-check"],
        "the splice must flatten disk-and-logs.toml's own step in after doctor-daily.toml's: {steps:?}"
    );
    assert!(steps.iter().all(|s| s["kind"] == "operation"));

    let effects = doc["effects"].as_array().unwrap();
    assert!(
        !effects.is_empty(),
        "a fresh FORGE2_HOME always has at least one WARN row (rate_limit: no samples yet): {doc:?}"
    );
    assert!(effects.iter().all(|e| e["kind"] == "row"), "{effects:?}");
    assert!(
        effects.iter().any(
            |e| e["target"] == "rate_limit" && e["summary"].as_str().unwrap().contains("WARN:")
        ),
        "{effects:?}"
    );
}

/// docs/JOBS.md step 5 ("The human rung"): a run workflow whose one step
/// always logs an effect and whose `[assert]` always fails, `on_failure =
/// "ask:operator"`. The failing run ends `needs_human` (the job analogue
/// of a blocked task) and files exactly one blocked no-work task on the
/// project, addressed to nobody in particular (the operator), whose
/// reason names the job's id, its workflow and the effect the run logged —
/// so `forge requests` surfaces it the way a failed deploy's question
/// already does.
#[test]
fn a_failing_assertion_with_ask_operator_leaves_one_blocked_task_naming_the_job_and_its_effect() {
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
        e.home.join("workflows/always-fails.toml"),
        r#"name = "always-fails"
kind = "run"
description = "writes a file and always fails its assertion, for the on_failure e2e"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[assert]
clean = ["bash", "-c", "exit 1"]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "ask:operator"
"#,
    )
    .unwrap();

    let input = e.home.join("input.json");
    std::fs::write(&input, r#"{"path":"out.txt","content":"hello"}"#).unwrap();
    let input_s = input.to_str().unwrap();

    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "always-fails",
            "--input",
            input_s,
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(doc["state"], "needs_human", "{doc:?}");
    let effects = doc["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0]["kind"], "file");

    let requests: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["requests", "--json"]).stdout).unwrap();
    let requests = requests.as_array().unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let r = &requests[0];
    assert!(r["to"].is_null(), "addressed to the operator: {r:?}");
    let text = r["text"].as_str().unwrap();
    assert!(text.contains(&format!("job {id}")), "{text:?}");
    assert!(text.contains("always-fails"), "{text:?}");
    assert!(text.contains("wrote 5 byte(s) to out.txt"), "{text:?}");
}

/// docs/JOBS.md step 5 ("The human rung"): the same always-fails fixture,
/// but `on_failure = "retry:1"` — one retry allowed. The first run fails
/// and requeues a second job with the same input; the worker claims and
/// runs that one too, which fails again and, its one retry already spent,
/// stops: two jobs recorded for the workflow, no third, and no blocked
/// task (this policy never asks).
#[test]
fn retry_1_runs_the_job_twice_then_stops() {
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
        e.home.join("workflows/always-fails-retry.toml"),
        r#"name = "always-fails-retry"
kind = "run"
description = "writes a file and always fails its assertion, for the on_failure retry e2e"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[assert]
clean = ["bash", "-c", "exit 1"]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "retry:1"
"#,
    )
    .unwrap();

    let input = e.home.join("input.json");
    std::fs::write(&input, r#"{"path":"out.txt","content":"hello"}"#).unwrap();
    let input_s = input.to_str().unwrap();

    // The first run, inline: fails, and requeues a retry.
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "always-fails-retry",
            "--input",
            input_s,
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let first: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();

    let jobs: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "list", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    let jobs = jobs.as_array().unwrap();
    assert_eq!(
        jobs.len(),
        2,
        "the first failure must have queued a retry: {jobs:?}"
    );
    let second = jobs
        .iter()
        .map(|j| j["id"].as_i64().unwrap())
        .find(|id| *id != first)
        .unwrap();

    let first_doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &first.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(first_doc["state"], "failed", "{first_doc:?}");
    assert_eq!(first_doc["retry_count"], 0, "{first_doc:?}");

    let second_doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &second.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(second_doc["state"], "queued", "{second_doc:?}");
    assert_eq!(second_doc["retry_count"], 1, "{second_doc:?}");

    // The worker claims and runs the retry: it fails too, and with its
    // one retry already spent, does not requeue a third.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let second_doc: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &second.to_string(), "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(second_doc["state"], "failed", "{second_doc:?}");

    let jobs: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["job", "list", "equitizr", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(
        jobs.as_array().unwrap().len(),
        2,
        "no third job: the retry budget was already spent: {jobs:?}"
    );

    assert!(
        e.forge("ok.sh", &["requests", "--json"])
            .stdout
            .starts_with(b"[]"),
        "retry never asks"
    );
}
