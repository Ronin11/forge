use super::*;

/// `events.dropped` naming one task and two jobs: `logs` reports each
/// count on its own, since a job's drops are marked `job:<id>` and must
/// not collapse into (or be missed by) the task tally.
#[test]
fn check_logs_counts_task_and_job_drops_separately() {
    let (_dir, f) = fixture();
    std::fs::write(f.paths.home.join("events.jsonl"), b"{}\n").unwrap();
    std::fs::write(f.paths.home.join("events.dropped"), b"1\njob:2\njob:3\n").unwrap();

    let checks = check_logs(&f.paths);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "logs");
    assert!(checks[0].status == Status::Warn);
    assert!(
        checks[0].detail.contains("1 task(s) lost log lines"),
        "{}",
        checks[0].detail
    );
    assert!(
        checks[0].detail.contains("2 job(s) lost log lines"),
        "{}",
        checks[0].detail
    );
}

/// A `Forge` over a fresh, empty store in a throwaway home (the same
/// fixture shape `view.rs`'s tests use).
fn fixture() -> (tempfile::TempDir, Forge) {
    use crate::ctx::Paths;

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let paths = Paths {
        worktrees: home.join("worktrees"),
        logs: home.join("logs"),
        home,
    };
    std::fs::create_dir_all(&paths.worktrees).unwrap();
    std::fs::create_dir_all(&paths.logs).unwrap();
    let store = Store::open(&paths.home.join("forge.db")).unwrap();
    let f = Forge::open_with(paths, store).unwrap();
    (dir, f)
}

fn fixture_task(
    state: TaskState,
    reason: &str,
    initiative: i64,
    finished_at: Option<i64>,
) -> crate::store::Task {
    crate::store::Task {
        repo: "/repo".into(),
        task: "do the thing".into(),
        base_branch: "main".into(),
        model: "sonnet".into(),
        max_turns: 10,
        max_attempts: 1,
        timeout_secs: 60,
        state,
        reason: reason.into(),
        finished_at,
        created_at: crate::unix_now(),
        workflow: "direct".into(),
        project: Some("demo".into()),
        initiative: Some(initiative),
        ..Default::default()
    }
}

/// One workflow measured on two providers, 0/10 on one and 8/10 on the
/// other: the learning check warns once, for the first, naming it, and
/// says nothing about the second (averaged, 8/20 would have read as
/// failing for both).
#[test]
fn check_learning_warns_per_provider_not_on_the_average() {
    let (_dir, f) = fixture();
    let hash = workflows::load_all(&f.paths.home)
        .unwrap()
        .into_iter()
        .find(|w| w.name == "direct")
        .unwrap()
        .hash;
    for (provider, landed) in [("local", 0), ("anthropic", 8)] {
        for i in 0..10 {
            let state = if i < landed {
                TaskState::Succeeded
            } else {
                TaskState::Failed
            };
            let mut t = fixture_task(state, "", 0, Some(crate::unix_now()));
            t.provider = provider.into();
            t.workflow_hash = hash.clone();
            t.started_at = Some(crate::unix_now());
            t.id = f.store.insert_task(&t).unwrap();
            f.store.update_task(&t).unwrap();
        }
    }

    let checks = check_learning(&f.paths, &f.store);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "learning");
    assert!(checks[0].status == Status::Warn, "{}", checks[0].detail);
    let warned: Vec<&str> = checks[0].detail.split("; ").collect();
    assert_eq!(warned.len(), 1, "{}", checks[0].detail);
    assert!(
        warned[0].starts_with("direct verifies 0/10 on local"),
        "{}",
        checks[0].detail
    );
    assert!(
        !checks[0].detail.contains("anthropic"),
        "{}",
        checks[0].detail
    );
}

#[test]
fn check_initiatives_is_ok_with_none_held() {
    let (_dir, f) = fixture();
    let checks = check_initiatives(&f);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "initiatives");
    assert!(checks[0].status == Status::Ok);
    assert_eq!(checks[0].detail, "none held");
}

/// A held initiative (its trailing same-rule failures reached its
/// stop rule) with one task still queued behind the hold: WARN,
/// naming the initiative, the rule and streak, the queued count, and
/// a fix line naming `forge initiative set <id>`.
#[test]
fn check_initiatives_warns_for_a_held_initiative_and_names_it() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&crate::store::Project {
            name: "demo".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    let ini_id = f
        .store
        .create_initiative(&crate::store::Initiative {
            project: "demo".into(),
            outcome: "o".into(),
            stop_after_same_rule: 2,
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    for _ in 0..2 {
        let mut t = fixture_task(
            TaskState::Failed,
            "L0 failed: has-commits (after 1 attempt(s))",
            ini_id,
            Some(crate::unix_now()),
        );
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
    }
    let mut queued = fixture_task(TaskState::Queued, "", ini_id, None);
    queued.id = f.store.insert_task(&queued).unwrap();
    f.store.update_task(&queued).unwrap();

    let checks = check_initiatives(&f);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "initiatives");
    assert!(checks[0].status == Status::Warn);
    assert!(
        checks[0].detail.contains(&format!("initiative {ini_id}")),
        "{}",
        checks[0].detail
    );
    assert!(
        checks[0].detail.contains("stop rule: has-commits"),
        "{}",
        checks[0].detail
    );
    assert!(
        checks[0].detail.contains("1 task(s) queued"),
        "{}",
        checks[0].detail
    );
    assert!(
        checks[0]
            .hint
            .contains(&format!("forge initiative set {ini_id}")),
        "{}",
        checks[0].hint
    );
}
