use super::*;
use crate::doctor::check;
use crate::store::{Project, Store};

fn git(dir: &std::path::Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

/// A forge with a `forge` project over a one-commit repository.
fn fixture() -> (tempfile::TempDir, Forge) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["config", "user.email", "a@a.com"]);
    git(&repo, &["config", "user.name", "a"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    std::fs::write(repo.join("forge.toml"), "[checks]\ntest = [\"true\"]\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "base"]);
    let home = dir.path().join("home");
    let paths = crate::ctx::Paths {
        worktrees: home.join("worktrees"),
        logs: home.join("logs"),
        home: home.clone(),
    };
    std::fs::create_dir_all(&paths.worktrees).unwrap();
    std::fs::create_dir_all(&paths.logs).unwrap();
    let store = Store::open(&home.join("forge.db")).unwrap();
    store
        .create_project(&Project {
            name: PROJECT.into(),
            ..Default::default()
        })
        .unwrap();
    store
        .register_repo(
            PROJECT,
            &repo.canonicalize().unwrap().display().to_string(),
            None,
        )
        .unwrap();
    (dir, Forge::open_with(paths, store).unwrap())
}

const NOW: i64 = 1_791_590_400;

fn alert_decisions(f: &Forge, watch: &str) -> i64 {
    f.store
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM decisions WHERE kind=?1 AND answered_by='doctor'",
            [decision_kind(watch)],
            |r| r.get(0),
        )
        .unwrap()
}

fn close(f: &Forge, id: i64, state: &str) {
    f.store
        .lock()
        .execute(
            "UPDATE tasks SET state=?2 WHERE id=?1",
            rusqlite::params![id, state],
        )
        .unwrap();
}

#[test]
fn which_rows_are_work() {
    assert!(is_work(&check("anything", Status::Fail, "", "")));
    for name in REPAIRABLE {
        assert!(is_work(&check(name, Status::Warn, "", "")), "{name}");
    }
    assert!(!is_work(&check("prompt_cache", Status::Warn, "", "")));
    assert!(!is_work(&check("steps", Status::Warn, "", "")));
    assert!(!is_work(&check("supervisor", Status::Ok, "", "")));
}

#[test]
fn the_text_is_the_watch_its_detail_and_its_hint() {
    let row = check(
        "jobs_stale",
        Status::Warn,
        "job 3 failed",
        "forge job show 3",
    );
    let text = task_text(&row);
    assert!(text.contains("`jobs_stale`"), "{text}");
    assert!(text.contains("job 3 failed"), "{text}");
    assert!(text.contains("forge job show 3"), "{text}");
}

#[tokio::test]
async fn a_standing_condition_is_one_task_and_one_decision() {
    let (_d, f) = fixture();
    let rows = vec![check(
        "egress_model",
        Status::Fail,
        "api.anthropic.com refused",
        "grant it",
    )];
    let opened = sync(&f, &rows, NOW).await.unwrap();
    assert_eq!(opened.len(), 1);
    let t = f.store.task(opened[0]).unwrap().unwrap();
    assert_eq!(t.state, TaskState::Queued);
    assert!(t.task.contains("api.anthropic.com refused"));
    assert_eq!(alert_decisions(&f, "egress_model"), 1);
    // Every later pass, however long the condition stands, opens nothing.
    for hours in [0, 1, 5, 7] {
        let again = sync(&f, &rows, NOW + hours * 3600).await.unwrap();
        assert!(again.is_empty(), "{hours}h");
    }
    assert_eq!(alert_decisions(&f, "egress_model"), 1);
}

#[tokio::test]
async fn a_warn_without_a_concrete_repair_opens_nothing() {
    let (_d, f) = fixture();
    let rows = vec![check("prompt_cache", Status::Warn, "read 10%", "")];
    assert!(sync(&f, &rows, NOW).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_closed_task_is_reopened_only_after_six_hours_while_the_condition_stands() {
    let (_d, f) = fixture();
    let rows = vec![check("land_rate", Status::Warn, "landed none", "look")];
    let first = sync(&f, &rows, NOW).await.unwrap()[0];
    close(&f, first, "failed");
    assert!(sync(&f, &rows, NOW + 3600).await.unwrap().is_empty());
    let second = sync(&f, &rows, NOW + RECHECK_SECS).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_ne!(second[0], first);
    assert_eq!(alert_decisions(&f, "land_rate"), 2);
}

#[tokio::test]
async fn an_open_task_is_left_alone_when_rechecked() {
    let (_d, f) = fixture();
    let rows = vec![check(
        "supervisor",
        Status::Warn,
        "none succeeded",
        "read it",
    )];
    sync(&f, &rows, NOW).await.unwrap();
    assert!(
        sync(&f, &rows, NOW + RECHECK_SECS)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(alert_decisions(&f, "supervisor"), 1);
}

#[tokio::test]
async fn a_watch_going_ok_marks_the_key_handled_and_a_new_trip_opens_again() {
    let (_d, f) = fixture();
    let bad = vec![check("jobs_stale", Status::Warn, "job 1", "fix")];
    let ok = vec![check("jobs_stale", Status::Ok, "fine", "")];
    let first = sync(&f, &bad, NOW).await.unwrap()[0];
    sync(&f, &ok, NOW + 60).await.unwrap();
    let m = f.store.doctor_mark("alert.jobs_stale").unwrap().unwrap();
    assert_eq!(m.status, "handled");
    // The nobody-started task is moot once the condition is gone.
    assert_eq!(
        f.store.task(first).unwrap().unwrap().state,
        TaskState::Withdrawn
    );
    // Handled and still OK writes nothing.
    sync(&f, &ok, NOW + 120).await.unwrap();
    assert_eq!(
        f.store.doctor_mark("alert.jobs_stale").unwrap().unwrap().at,
        NOW + 60
    );
    let again = sync(&f, &bad, NOW + 180).await.unwrap();
    assert_eq!(again.len(), 1);
    assert_ne!(again[0], first);
}

#[tokio::test]
async fn a_handled_key_whose_capped_task_is_still_open_is_reopened_not_refiled() {
    let (_d, f) = fixture();
    let bad = vec![check("jobs_stale", Status::Warn, "job 1", "fix")];
    let ok = vec![check("jobs_stale", Status::Ok, "fine", "")];
    let first = sync(&f, &bad, NOW).await.unwrap()[0];
    close(&f, first, "capped");
    sync(&f, &ok, NOW + 60).await.unwrap();
    let m = f.store.doctor_mark("alert.jobs_stale").unwrap().unwrap();
    assert_eq!(m.status, "handled");
    assert_eq!(
        f.store.task(first).unwrap().unwrap().state,
        TaskState::Capped
    );
    let again = sync(&f, &bad, NOW + 120).await.unwrap();
    assert!(again.is_empty());
    let m = f.store.doctor_mark("alert.jobs_stale").unwrap().unwrap();
    assert_eq!(m.status, "open");
    assert_eq!(m.detail, first.to_string());
    assert_eq!(alert_decisions(&f, "jobs_stale"), 1);
}

#[tokio::test]
async fn the_worker_tick_runs_the_watches_and_leaves_the_last_run_mark_alone() {
    let (_d, f) = fixture();
    let opened = tick(&f).await.unwrap();
    assert!(opened.is_empty());
    assert!(f.store.doctor_mark("doctor.last_run").unwrap().is_none());
    // The watches' own marks are written on the first pass...
    assert!(f.store.doctor_mark("watch.steps").unwrap().is_some());
    // ...and a second pass changes nothing.
    let before = f.store.doctor_mark("watch.steps").unwrap().unwrap();
    tick(&f).await.unwrap();
    assert_eq!(f.store.doctor_mark("watch.steps").unwrap().unwrap(), before);
}
