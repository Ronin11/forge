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
