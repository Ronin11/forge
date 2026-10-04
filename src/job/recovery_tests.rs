//! Regression coverage for requeued runs and terminal recovery.
use super::*;
use crate::ctx::Paths;
use crate::store::{Project, Store};

fn setup() -> (tempfile::TempDir, Forge, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("forge.db")).unwrap();
    store
        .create_project(&Project {
            name: "recovery".into(),
            purpose: "test".into(),
            ..Default::default()
        })
        .unwrap();
    let id = store
        .create_job(&Job {
            project: "recovery".into(),
            state: JobState::Running,
            ..Default::default()
        })
        .unwrap();
    let f = Forge::open_with(
        Paths {
            home: dir.path().into(),
            worktrees: dir.path().join("worktrees"),
            logs: dir.path().join("logs"),
        },
        store,
    )
    .unwrap();
    (dir, f, id)
}

fn paid_step(f: &Forge, id: i64, seq: i64, output: &Path) {
    f.store
        .append_job_step(&JobStep {
            job_id: id,
            seq,
            cost_usd: Some(0.02),
            output_ref: output.display().to_string(),
            ..Default::default()
        })
        .unwrap();
}

#[test]
fn requeued_runs_keep_outputs_order_and_total_cost() {
    let (_dir, f, id) = setup();
    let first = recovery::prepare_run(&f, id, "{}")
        .unwrap()
        .join("step.txt");
    std::fs::write(&first, "first output").unwrap();
    paid_step(&f, id, 0, &first);
    paid_step(&f, id, 1, &first);
    recover_interrupted(&f, id, &Owner::this_process()).unwrap();
    assert_eq!(f.store.job_run(id).unwrap(), 1);
    f.store.requeue_job(id, &Owner::this_process()).unwrap(); // A queued job must not advance twice.
    assert_eq!(f.store.job_run(id).unwrap(), 1);
    f.store.claim_next_job().unwrap().unwrap();
    let second = recovery::prepare_run(&f, id, "{}")
        .unwrap()
        .join("step.txt");
    std::fs::write(&second, "second output").unwrap();
    paid_step(&f, id, 0, &second);
    let steps = f.store.job_steps(id).unwrap();
    assert_eq!(
        steps.iter().map(|s| s.run).collect::<Vec<_>>(),
        [0, 0, 0, 1]
    );
    assert_eq!(
        std::fs::read_to_string(&steps[0].output_ref).unwrap(),
        "first output"
    );
    assert_eq!(
        std::fs::read_to_string(&steps[3].output_ref).unwrap(),
        "second output"
    );
    f.store
        .finish_job(id, 1, JobState::Ok, Some(0.02), "[]")
        .unwrap();
    assert_eq!(f.store.job(id).unwrap().unwrap().cost_usd, Some(0.06));
}

#[test]
fn recovery_finishes_and_emits_even_when_asking_fails() {
    let (_dir, f, id) = setup();
    let run = recovery::prepare_run(&f, id, "{}").unwrap();
    paid_step(&f, id, 0, &run.join("step.json"));
    std::fs::write(run.join("effects.log"), "message\talice\tsent\n").unwrap();
    // A failed question insert simulates stopping at the ask boundary.
    rusqlite::Connection::open(f.paths.home.join("forge.db")).unwrap().execute_batch("CREATE TRIGGER refuse_question BEFORE INSERT ON tasks BEGIN SELECT RAISE(FAIL, 'ask unavailable'); END;").unwrap();
    assert!(recover_interrupted(&f, id, &Owner::this_process()).is_err());
    let job = f.store.job(id).unwrap().unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.cost_usd, Some(0.02));
    assert!(
        f.store
            .orphan_jobs(&crate::store::Caller::this_process(false), |_| false)
            .unwrap()
            .is_empty()
    );
    let events = std::fs::read_to_string(f.paths.home.join("events.jsonl")).unwrap();
    assert!(events.contains("job_finished"));
    assert!(events.contains("0.02"));
}

#[tokio::test]
async fn rerun_budget_includes_interrupted_steps() {
    let (dir, f, id) = setup();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    paid_step(&f, id, 0, Path::new("paid.json"));
    recover_interrupted(&f, id, &Owner::this_process()).unwrap();
    f.store.claim_next_job().unwrap().unwrap();
    let limits = workflows::Limits {
        budget_usd: 0.01,
        per_day: 10,
        on_failure: workflows::OnFailure::Drop,
        input_bytes: 1024,
    };
    run_now(RunNow {
        f: &f,
        job_id: id,
        project: "recovery",
        workflow: "test",
        repo: &repo,
        landed_sha: "HEAD",
        steps: &[],
        assert: &BTreeMap::new(),
        skip_if: &BTreeMap::new(),
        limits: Some(&limits),
        trigger: None,
        workflow_env: &BTreeMap::new(),
        dry_run: true,
        input_text: "{}",
        input_fields: &[],
        check_timeout_secs: 10,
        project_roles: &BTreeMap::new(),
        recorded: &BTreeMap::new(),
    })
    .await
    .unwrap();
    let job = f.store.job(id).unwrap().unwrap();
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.cost_usd, Some(0.02));
    let verdict: serde_json::Value = serde_json::from_str(&job.verdict_json).unwrap();
    assert!(
        verdict
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "budget" && v["ok"] == false)
    );
}

#[tokio::test]
async fn a_job_with_a_broken_route_needs_human_without_a_failed_check() {
    let (dir, mut f, id) = setup();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "not a directory").unwrap();
    let proxies = blocker.join("proxies");
    let mut sandbox =
        crate::sandbox::Sandbox::with_bwrap("/bin/false".into(), dir.path().join("sandbox-home"));
    sandbox.proxies = Arc::new(crate::egress::Proxies::in_dir(proxies.clone()));
    f.sandbox = Some(crate::executor::Execution::bwrap_only(sandbox));
    f.store.set_job_trust(id, Trust::Operator).unwrap();
    let step = workflows::RunStep {
        action: toml::from_str(
            r#"name = "route-test"
kind = "operation"
description = "must not launch without a route"
run = ["/bin/true"]
"#,
        )
        .unwrap(),
        role: None,
        model: None,
        max_turns: None,
        timeout_secs: None,
        effect: None,
        node: "0-route-test".into(),
        on: BTreeMap::new(),
        max_attempts: 1,
        secrets: vec![],
        egress: vec!["example.com:443".into()],
        budget_usd: None,
    };
    let error = run_now(RunNow {
        f: &f,
        job_id: id,
        project: "recovery",
        workflow: "route-test",
        repo: &repo,
        landed_sha: "HEAD",
        steps: &[step],
        assert: &BTreeMap::new(),
        skip_if: &BTreeMap::new(),
        limits: None,
        trigger: None,
        workflow_env: &BTreeMap::new(),
        dry_run: false,
        input_text: "{}",
        input_fields: &[],
        check_timeout_secs: 10,
        project_roles: &BTreeMap::new(),
        recorded: &BTreeMap::new(),
    })
    .await
    .expect_err("a broken route must remain an environment error");
    assert!(format!("{error:#}").contains(&proxies.display().to_string()));
    use crate::engine::{Classify, Fault};
    assert!(matches!(Err::<(), _>(error).task(), Err(Fault::Env(_))));
    let job = f.store.job(id).unwrap().unwrap();
    assert_eq!(job.state, JobState::NeedsHuman);
    assert!(job.finished_at.is_some());
    assert_eq!(job.verdict_json, "[]");
    assert!(f.store.job_steps(id).unwrap().is_empty());
    let reason: String = rusqlite::Connection::open(f.paths.home.join("forge.db"))
        .unwrap()
        .query_row(
            "SELECT reason FROM tasks WHERE task = 'job question'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(reason.contains("environment fault"));
    assert!(reason.contains(&proxies.display().to_string()));
}
