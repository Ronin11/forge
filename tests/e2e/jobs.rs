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
