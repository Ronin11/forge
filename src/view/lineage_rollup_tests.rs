use super::*;
use crate::ctx::Paths;
use crate::store::{Initiative, Project, Store};

/// A `Forge` over a fresh, empty store in a throwaway home.
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

fn fixture_task(project: &str, state: TaskState, retry_of: Option<i64>, initiative: i64) -> Task {
    Task {
        repo: "/repo".into(),
        task: "do the thing".into(),
        base_branch: "main".into(),
        model: "sonnet".into(),
        max_turns: 10,
        max_attempts: 1,
        timeout_secs: 60,
        state,
        created_at: crate::unix_now(),
        workflow: "direct".into(),
        project: Some(project.into()),
        initiative: Some(initiative),
        retry_of,
        ..Default::default()
    }
}

fn insert(f: &Forge, mut t: Task) -> Task {
    t.id = f.store.insert_task(&t).unwrap();
    f.store.update_task(&t).unwrap();
    t
}

/// A lineage of three (blocked, then failed, then a retry that
/// landed) must count once, as its latest task's state: succeeded,
/// not also blocked and failed. The report names the same lineage by
/// its latest task and says how many retries it took to land.
#[test]
fn a_landed_retry_counts_its_lineage_once_as_succeeded() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "demo".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    let ini_id = f
        .store
        .create_initiative(&Initiative {
            project: "demo".into(),
            outcome: "o".into(),
            stop_after_same_rule: 3,
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    let t1 = insert(&f, fixture_task("demo", TaskState::Blocked, None, ini_id));
    let t2 = insert(
        &f,
        fixture_task("demo", TaskState::Failed, Some(t1.id), ini_id),
    );
    let mut t3 = fixture_task("demo", TaskState::Succeeded, Some(t2.id), ini_id);
    t3.finished_at = Some(crate::unix_now());
    let t3 = insert(&f, t3);

    let ini = f.store.initiative(ini_id).unwrap().unwrap();
    let row = initiative_row(&f, &ini).unwrap();
    assert_eq!(row.succeeded, 1, "the lineage's latest task landed");
    assert_eq!(row.blocked, 0);
    assert_eq!(row.failed, 0);
    assert_eq!(row.state, "done");

    let doc = initiative_doc(&f, &ini).unwrap();
    assert_eq!(doc.tasks.len(), 1);
    assert_eq!(doc.tasks[0].id, t3.id);
    assert_eq!(doc.tasks[0].state, "succeeded");
    assert_eq!(doc.tasks[0].retries, 2, "it took two retries to land");
}

/// `forge project show`'s task counts collapse the same way: a
/// project-scoped task that landed on retry counts once, as succeeded.
#[test]
fn project_show_counts_the_same_lineage_once() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "demo".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    // No initiative: these are standalone project tasks.
    let t1 = insert(
        &f,
        Task {
            initiative: None,
            ..fixture_task("demo", TaskState::Blocked, None, 0)
        },
    );
    insert(
        &f,
        Task {
            initiative: None,
            ..fixture_task("demo", TaskState::Succeeded, Some(t1.id), 0)
        },
    );

    let p = f.store.project("demo").unwrap().unwrap();
    let row = project_row(&f, &p).unwrap();
    assert_eq!(row.succeeded, 1);
    assert_eq!(row.blocked, 0);
}
