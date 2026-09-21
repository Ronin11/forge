//! The client contract's checked-in examples (docs/CLIENT.md, "Fixtures"):
//! `tests/fixtures/job.json`, `deploy.json` and `portal.json`, real
//! `forge` output captured once and parsed here with `forge-client`'s own
//! types, so a shape drift between what the CLI prints and what
//! `JobDoc`/`Deploy`/`PortalDoc` expect fails a test instead of a client
//! at runtime.

use crate::support::*;
use forge_client::{Deploy, JobDoc, PortalDoc};
use std::path::{Path, PathBuf};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Parse `created initiative N` from `forge initiative new`'s stdout.
fn created_initiative_id(o: &std::process::Output) -> i64 {
    let out = String::from_utf8_lossy(&o.stdout);
    out.lines()
        .find_map(|l| l.strip_prefix("created initiative "))
        .unwrap_or_else(|| panic!("{out}"))
        .parse()
        .unwrap()
}

const BRIEF_JSON: &str = r#"{
    "workflows": [
        {"name": "quote by photo", "trigger": "a photo comes in", "inputs": "a photo",
         "outputs": "a quote", "other_people": "none", "failure_today": "nothing",
         "success_signal": "a quote sent", "do_not_touch": "billing"},
        {"name": "weekly invoice", "trigger": "friday", "inputs": "jobs done",
         "outputs": "an invoice", "other_people": "none", "failure_today": "nothing",
         "success_signal": "invoice sent", "do_not_touch": "billing"}
    ],
    "where_it_runs": "local, deploy-command",
    "confirmed": true
}"#;

/// Regenerates the three fixtures below from real `forge` output on a
/// throwaway project: never runs as part of `cargo test --workspace`
/// (`#[ignore]`), so the checked-in files only change when a person
/// deliberately reruns it and reviews the diff, e.g.:
///
///   cargo test --test e2e -- --ignored capture_job_deploy_and_portal_fixtures --nocapture
///
/// then `git diff tests/fixtures/` before committing it.
#[test]
#[ignore]
fn capture_job_deploy_and_portal_fixtures() {
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
                "quote requests turned into automations",
                "--repo",
                repo_s,
            ],
        )
        .status
        .success()
    );

    // job.json: `forge job show --json` for a real run workflow's real
    // `--now` run (the two built-in effect operations, write-file and
    // append-row, both actually executed).
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(
        e.home.join("workflows/nightly-sync.toml"),
        r#"name = "nightly-sync"
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
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "equitizr",
            "nightly-sync",
            "--input",
            input.to_str().unwrap(),
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let job_id: i64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap();
    let job_doc = e.forge("ok.sh", &["job", "show", &job_id.to_string(), "--json"]);
    assert!(job_doc.status.success());
    std::fs::write(fixture_path("job.json"), &job_doc.stdout).unwrap();

    // deploy.json: `forge deploy log --json` after a passing deploy and a
    // failing one that rolls back to it, both real `deploy-command` runs
    // against `host = local` (real rsync, no network).
    std::fs::write(e.repo.join("flag.txt"), "good\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "good"]);
    let good_sha = git(&e.repo, &["rev-parse", "HEAD"]);

    std::fs::write(e.repo.join("flag.txt"), "bad\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "bad"]);
    let bad_sha = git(&e.repo, &["rev-parse", "HEAD"]);

    let dest = e._dir.path().join("site");
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "deploy",
                "add",
                "equitizr",
                "prod",
                "--repo",
                repo_s,
                "--method",
                "deploy-command",
                "--arg",
                "host=local",
                "--arg",
                &format!("dest={}", dest.to_str().unwrap()),
                "--arg",
                "command=true",
                "--check",
                "cat flag.txt; grep -qx good flag.txt",
            ],
        )
        .status
        .success()
    );

    let o = e.forge("ok.sh", &["deploy", "equitizr", "prod", "--sha", &good_sha]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.forge("ok.sh", &["deploy", "equitizr", "prod", "--sha", &bad_sha]);
    assert!(!o.status.success(), "the bad commit's check should fail");

    let deploy_log = e.forge("ok.sh", &["deploy", "log", "equitizr", "prod", "--json"]);
    assert!(deploy_log.status.success());
    std::fs::write(fixture_path("deploy.json"), &deploy_log.stdout).unwrap();

    // portal.json: `forge project view --json`. The deploy target and job
    // above already populate `deploy_targets` and `run_workflows`; the
    // rest (a backlog item, an open initiative with a question blocking
    // it, a landed task, a confirmed intake brief) is filed the same way
    // `src/view.rs`'s own `portal_doc_never_carries_a_forbidden_key_...`
    // unit test builds a project with everything, since driving a real
    // agent through an intake interview and a full landing just to shake
    // the same rows loose is not what this fixture is for.
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "backlog",
                "equitizr",
                "--add",
                "send a weekly summary",
            ],
        )
        .status
        .success()
    );

    let o = e.forge(
        "ok.sh",
        &[
            "initiative",
            "new",
            "equitizr",
            "--outcome",
            "quoting takes one click",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let ini_id = created_initiative_id(&o);

    {
        let db = e.db();
        db.execute(
            "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, reason, created_at, workflow, project, initiative)
             VALUES (?1, 'which price sheet should this pull from?', 'main', 'sonnet', 10, 1, 60, 'blocked', 'needs input: which price sheet should this pull from?', 1, 'direct', 'equitizr', ?2)",
            rusqlite::params![repo_s, ini_id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO tasks (repo, task, base_branch, branch, model, max_turns, max_attempts, timeout_secs, state, created_at, finished_at, landed_sha, workflow, project)
             VALUES (?1, ?2, 'main', 'task-1-branch', 'sonnet', 10, 1, 60, 'succeeded', 1, 2, 'deadbeef', 'direct', 'equitizr')",
            rusqlite::params![
                repo_s,
                "Make the quote text say 'usually same day'. Implementation: update src/pricing/quote.rs around line 42.",
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs, state, created_at, workflow, project, plan)
             VALUES (?1, 'intake interview', 'main', 'sonnet', 10, 1, 60, 'succeeded', 1, 'intake', 'equitizr', ?2)",
            rusqlite::params![repo_s, BRIEF_JSON],
        )
        .unwrap();
    }

    let portal = e.forge("ok.sh", &["project", "view", "equitizr", "--json"]);
    assert!(
        portal.status.success(),
        "{}",
        String::from_utf8_lossy(&portal.stderr)
    );
    std::fs::write(fixture_path("portal.json"), &portal.stdout).unwrap();
}

#[test]
fn job_fixture_parses_as_a_jobdoc_with_the_fields_a_client_reads() {
    let raw = std::fs::read_to_string(fixture_path("job.json")).unwrap();
    let doc: JobDoc = serde_json::from_str(&raw).unwrap();
    assert_eq!(doc.workflow, "nightly-sync");
    assert_eq!(doc.state, "ok");
    assert!(!doc.dry_run);
    assert_eq!(doc.cost_usd, Some(0.0));
    assert!(!doc.verdict_json.is_empty());

    let steps = &doc.steps;
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert_eq!(steps[0].action, "write-file");
    assert_eq!(steps[0].kind, "operation");
    assert_eq!(steps[1].action, "append-row");

    let effects = &doc.effects;
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert_eq!(effects[0].kind, "file");
    assert_eq!(effects[0].target, "out.txt");
    assert!(!effects[0].dry_run);
    assert_eq!(effects[1].kind, "row");
    assert_eq!(effects[1].target, "book.csv");
    assert!(!effects[1].dry_run);
}

#[test]
fn deploy_fixture_parses_as_deploy_rows_with_the_fields_a_client_reads() {
    let raw = std::fs::read_to_string(fixture_path("deploy.json")).unwrap();
    let rows: Vec<Deploy> = serde_json::from_str(&raw).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");

    // Newest first: the failing bad_sha deploy, then the passing good_sha one.
    let failed = &rows[0];
    assert_eq!(failed.project, "equitizr");
    assert_eq!(failed.target, "prod");
    assert_eq!(failed.check_ok, Some(false));
    assert!(
        failed.rolled_back_to.is_some(),
        "a failed check rolls back to the last passing sha"
    );
    assert!(!failed.check_output.is_empty());

    let passed = &rows[1];
    assert_eq!(passed.check_ok, Some(true));
    assert_eq!(passed.rolled_back_to, None);
    assert!(passed.finished_at.is_some());
}

#[test]
fn portal_fixture_parses_as_a_portaldoc_with_the_fields_a_client_reads() {
    let raw = std::fs::read_to_string(fixture_path("portal.json")).unwrap();
    let doc: PortalDoc = serde_json::from_str(&raw).unwrap();
    assert_eq!(doc.project, "equitizr");

    assert_eq!(doc.deploy_targets.len(), 1);
    assert_eq!(doc.deploy_targets[0].name, "prod");
    assert_eq!(doc.deploy_targets[0].where_it_runs, "local");
    assert_eq!(
        doc.deploy_targets[0].check_ok,
        Some(false),
        "the most recent of the two deploys captured for deploy.json is the failing one"
    );

    assert_eq!(doc.run_workflows.len(), 1);
    assert_eq!(doc.run_workflows[0].name, "nightly-sync");
    assert_eq!(doc.run_workflows[0].jobs.len(), 1);
    assert_eq!(doc.run_workflows[0].jobs[0].state, "ok");

    assert_eq!(doc.initiatives.len(), 1);
    assert_eq!(doc.initiatives[0].outcome, "quoting takes one click");
    assert_eq!(
        doc.initiatives[0].state, "waiting on you",
        "its only task is blocked on a question"
    );
    assert_eq!(doc.initiatives[0].pieces, 1);

    assert_eq!(doc.questions.len(), 1);
    assert_eq!(
        doc.questions[0].text,
        "which price sheet should this pull from?"
    );
    assert!(
        doc.questions[0].asked_at.is_some_and(|t| t > 0),
        "a question says when its task blocked on it"
    );

    assert_eq!(doc.landed.len(), 1);
    assert_eq!(
        doc.landed[0].text,
        "Make the quote text say 'usually same day'."
    );
    assert_eq!(doc.landed[0].pieces, None);

    assert_eq!(doc.backlog.len(), 1);
    assert_eq!(doc.backlog[0].text, "send a weekly summary");

    let brief = doc.brief.expect("a confirmed brief was filed");
    assert_eq!(brief.where_it_runs, "local, deploy-command");
    assert_eq!(brief.workflows.len(), 2);
    assert!(brief.workflows[0].starts_with("quote by photo:"));
}
