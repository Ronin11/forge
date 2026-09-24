use super::*;
use crate::ctx::Paths;
use crate::store::{Store, TaskState};

fn fixture() -> (tempfile::TempDir, Forge) {
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

/// A landed task on `workflow` that took `secs` from creation to landing.
fn landed(f: &Forge, workflow: &str, hash: &str, secs: i64) -> Task {
    let mut t = Task {
        repo: "/repo".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: 1_000,
        started_at: Some(1_000),
        finished_at: Some(1_000 + secs),
        landed_sha: format!("sha-{workflow}"),
        landed_at: Some(1_000 + secs),
        workflow: workflow.into(),
        workflow_hash: hash.into(),
        ..Default::default()
    };
    t.id = f.store.insert_task(&t).unwrap();
    f.store.update_task(&t).unwrap();
    // The git-derived caches `stats_doc` refreshes, filled so the
    // refresh has nothing to compute against the fixture's fake repo.
    f.store.set_churn_cache(t.id, 0, 0, i64::MAX / 2).unwrap();
    f.store
        .set_repair_cost_cache(t.id, 0.0, i64::MAX / 2)
        .unwrap();
    f.store.set_hand_commits_cache(t.id, 0, 1).unwrap();
    t
}

#[tokio::test]
async fn stats_doc_carries_one_time_to_live_row_per_workflow() {
    let (_dir, f) = fixture();
    landed(&f, "direct", "h-direct", 100);
    landed(&f, "tdd", "h-tdd", 700);

    let doc = stats_doc(&f, &crate::store::StatsFilter::default(), None)
        .await
        .unwrap();
    assert_eq!(doc.time_to_live.len(), 2, "one row per workflow");
    let row = |wf: &str| doc.time_to_live.iter().find(|r| r.workflow == wf).unwrap();
    let (d, t) = (row("direct"), row("tdd"));
    assert_eq!((d.hash.as_str(), d.n), ("h-direct", 1));
    assert_eq!((d.median_secs, d.p90_secs), (Some(100.0), Some(100.0)));
    assert_eq!((t.hash.as_str(), t.n), ("h-tdd", 1));
    assert_eq!((t.median_secs, t.p90_secs), (Some(700.0), Some(700.0)));

    let v = serde_json::to_value(&doc).unwrap();
    assert_eq!(v["time_to_live"].as_array().unwrap().len(), 2, "{v}");
}
