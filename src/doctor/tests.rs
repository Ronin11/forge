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

/// A project whose repository path is gone (the worktree was cleaned up
/// out from under it) gets one clear `projects.<name>` row naming `forge
/// project retire`, and `executors` skips it rather than FAILing.
#[test]
fn a_project_with_a_missing_repo_gets_one_stale_row_and_executors_skips_it() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&crate::store::Project {
            name: "gone".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    f.store
        .register_repo("gone", "/no/such/repository/anywhere", None)
        .unwrap();

    let stale = check_stale_projects(&f.store);
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].name, "projects.gone");
    assert!(stale[0].status == Status::Warn);
    assert!(
        stale[0].hint.contains("forge project retire gone"),
        "{}",
        stale[0].hint
    );

    let executors = check_executors(&f.store, &f.paths);
    assert!(
        !executors
            .iter()
            .any(|c| c.name == "executors" && c.status == Status::Fail),
        "{:?}",
        executors.iter().map(|c| &c.detail).collect::<Vec<_>>()
    );
}

/// `forge project retire` leaves a retired project out of
/// `check_stale_projects` (and every other active-only pass), even though
/// its repository path is still gone: a retired project's absent
/// repository is no longer anyone's problem to report.
#[test]
fn a_retired_project_with_a_missing_repo_is_not_reported_stale() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&crate::store::Project {
            name: "gone".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    f.store
        .register_repo("gone", "/no/such/repository/anywhere", None)
        .unwrap();
    assert!(f.store.retire_project("gone", crate::unix_now()).unwrap());

    assert!(check_stale_projects(&f.store).is_empty());
    // Still reachable by name, with its repo and history intact.
    let kept = f.store.project("gone").unwrap().unwrap();
    assert!(kept.retired_at.is_some());
    assert_eq!(f.store.project_repos("gone").unwrap().len(), 1);
}
