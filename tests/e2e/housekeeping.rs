//! Worktree lifecycle: a landed task's tree goes at landing, every other
//! finished task's is kept `worktree_retention_days` and then published and
//! removed, and the worker does that housekeeping itself.
use crate::support::*;
use std::path::PathBuf;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Task `id` finished `days` days ago, as far as the clock is concerned.
fn finished_days_ago(e: &Env, id: i64, days: i64) {
    e.db()
        .execute(
            "UPDATE tasks SET finished_at=?1 WHERE id=?2",
            rusqlite::params![now() - days * 86_400, id],
        )
        .unwrap();
}

fn removed_at(e: &Env, id: i64) -> Option<i64> {
    e.db()
        .query_row(
            "SELECT worktree_removed_at FROM tasks WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

fn age_dir(dir: &std::path::Path, days: u64) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    std::fs::File::open(dir).unwrap().set_modified(old).unwrap();
}

fn stderr_of(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn landing_removes_the_worktree_and_every_sibling_and_marks_it() {
    let e = Env::new();
    let wts = e.home.join("worktrees");
    for sibling in ["1-overlays", "1-tmp", "1-review-provider"] {
        std::fs::create_dir_all(wts.join(sibling).join("junk")).unwrap();
        std::fs::write(wts.join(sibling).join("junk/data"), "x").unwrap();
    }
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", stderr_of(&o));
    assert!(e.task(1).1.starts_with("landed main @ "));
    for gone in [
        "1",
        "1-overlays",
        "1-tmp",
        "1-review-provider",
        "1-provider",
        "1-tests",
        "landing-1",
    ] {
        assert!(!wts.join(gone).exists(), "{gone} survived landing");
    }
    assert!(removed_at(&e, 1).is_some(), "worktree_removed_at is set");
}

#[test]
fn retention_keeps_a_failed_tree_then_publishes_its_leftovers_and_removes_it() {
    let e = Env::new();
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    let tree = e.home.join("worktrees/1");
    std::fs::write(tree.join("leftover.txt"), "unsaved").unwrap();

    // Young: the worker's housekeeping keeps it and says so.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(
        stderr_of(&o).contains("housekeeping: freed "),
        "{}",
        stderr_of(&o)
    );
    assert!(stderr_of(&o).contains("kept 1"), "{}", stderr_of(&o));
    assert!(tree.join("leftover.txt").exists());
    assert!(!e.origin_branches().contains("forge/1-"));

    // Past the default seven days: committed, pushed, removed.
    finished_days_ago(&e, 1, 8);
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(
        stderr_of(&o).contains("removed 1 trees"),
        "{}",
        stderr_of(&o)
    );
    assert!(!tree.exists());
    assert!(removed_at(&e, 1).is_some());
    let branch = e.origin_branches();
    let branch = branch
        .lines()
        .map(|l| l.trim().trim_start_matches("* "))
        .find(|l| l.starts_with("forge/1-"))
        .expect("the branch was published")
        .to_string();
    let log = git(&e.origin, &["log", "--format=%s", &branch]);
    assert!(
        log.lines().any(|l| l == "forge: leftovers at gc, task 1"),
        "{log}"
    );
    let files = git(&e.origin, &["ls-tree", "-r", "--name-only", &branch]);
    assert!(files.contains("leftover.txt"), "{files}");
}

#[test]
fn retention_days_come_from_the_workers_config() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[worker]\nworktree_retention_days = 2\n",
    )
    .unwrap();
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    finished_days_ago(&e, 1, 1);
    let tree = e.home.join("worktrees/1");
    e.forge("ok.sh", &["work", "--once"]);
    assert!(tree.exists(), "one day old is inside a two day retention");
    finished_days_ago(&e, 1, 3);
    e.forge("ok.sh", &["work", "--once"]);
    assert!(!tree.exists(), "three days old is past it");
}

#[test]
fn a_husk_and_an_orphan_sibling_are_removed() {
    let e = Env::new();
    let id = e.add(&[]);
    let wts = e.home.join("worktrees");
    // A tracked tree with no .git: a husk.
    let husk = wts.join(id.to_string());
    std::fs::create_dir_all(husk.join("target")).unwrap();
    std::fs::write(husk.join("target/x"), "x").unwrap();
    e.db()
        .execute(
            "UPDATE tasks SET state='failed', worktree=?1, finished_at=?2 WHERE id=?3",
            rusqlite::params![husk.to_string_lossy(), now(), id],
        )
        .unwrap();
    // A sibling whose task directory is gone, and a landing tree nobody tracks.
    let orphan: PathBuf = wts.join("77-overlays");
    std::fs::create_dir_all(orphan.join("work/work")).unwrap();
    let landing = wts.join("landing-78");
    std::fs::create_dir_all(landing.join("target")).unwrap();
    std::fs::write(landing.join("target/x"), "x").unwrap();
    age_dir(&landing, 2);

    let o = e.forge("ok.sh", &["gc"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains(&format!("task {id:<4} removed")), "{out}");
    assert!(!husk.exists());
    assert!(removed_at(&e, id).is_some());
    assert!(!orphan.exists(), "{out}");
    assert!(!landing.exists(), "{out}");
}

#[test]
fn gc_dry_run_names_what_it_would_do_and_does_none_of_it() {
    let e = Env::new();
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    let tree = e.home.join("worktrees/1");
    std::fs::write(tree.join("leftover.txt"), "unsaved").unwrap();
    let orphan = e.home.join("worktrees/9-tmp");
    std::fs::create_dir_all(&orphan).unwrap();
    finished_days_ago(&e, 1, 8);

    let o = e.forge("ok.sh", &["gc", "--dry-run"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("task 1    would remove"), "{out}");
    assert!(
        out.contains("would commit leftovers, publish the branch"),
        "{out}"
    );
    assert!(out.contains("orphan siblings would remove"), "{out}");
    assert!(tree.join("leftover.txt").exists());
    assert!(orphan.exists());
    assert!(removed_at(&e, 1).is_none());
    assert!(!e.origin_branches().contains("forge/1-"));
}

#[test]
fn doctor_reports_worktree_size_and_trees_past_retention() {
    let e = Env::new();
    assert!(!e.run("wrong.sh", &["--retries", "0"]).status.success());
    finished_days_ago(&e, 1, 9);
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("total "), "{out}");
    assert!(out.contains("1 retained past 7d retention"), "{out}");
}
