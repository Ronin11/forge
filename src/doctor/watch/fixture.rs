//! A fixture store the watches' tests write rows into directly: the
//! rows each watch reads, and nothing it does not.

use crate::store::Store;
use rusqlite::params;

pub const NOW: i64 = 1_791_590_400; // 2026-10-10 00:00 UTC

pub fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("forge.db")).unwrap();
    (dir, s)
}

/// A finished task of `project` on `branch` in `state`.
pub fn task(s: &Store, project: &str, branch: &str, state: &str) -> i64 {
    let c = s.lock();
    c.execute(
        "INSERT INTO tasks (repo, task, base_branch, model, max_turns, max_attempts, timeout_secs,
                            state, created_at, finished_at, project, branch)
         VALUES ('/repo', 't', 'main', 'm', 1, 1, 1, ?1, ?2, ?2, ?3, ?4)",
        params![state, NOW - 3600, project, branch],
    )
    .unwrap();
    c.last_insert_rowid()
}

/// An attempt at `step` of `task`, finished at `at`.
pub fn attempt(s: &Store, task: i64, step: &str, state: &str, reason: &str, at: i64) -> i64 {
    attempt_with(s, task, step, state, reason, at, "[]", "{}")
}

#[allow(clippy::too_many_arguments)]
pub fn attempt_with(
    s: &Store,
    task: i64,
    step: &str,
    state: &str,
    reason: &str,
    at: i64,
    verdict_json: &str,
    outputs_json: &str,
) -> i64 {
    let c = s.lock();
    c.execute(
        "INSERT INTO attempts (task_id, attempt_no, state, reason, started_at, finished_at, step,
                               verdict_json, outputs_json)
         VALUES (?1, 1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)",
        params![task, state, reason, at, step, verdict_json, outputs_json],
    )
    .unwrap();
    c.last_insert_rowid()
}

/// A kernel operation `name` of `task`, started at `at`.
pub fn op(s: &Store, task: i64, name: &str, ok: bool, detail: &str, at: i64) {
    s.lock()
        .execute(
            "INSERT INTO ops (task_id, seq, name, kernel, started_at, ok, detail)
             VALUES (?1, 0, ?2, 1, ?3, ?4, ?5)",
            params![task, name, at, ok as i64, detail],
        )
        .unwrap();
}
