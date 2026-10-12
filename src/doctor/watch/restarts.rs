//! Watch 2, `worker_restarts`: more than three worker starts since the
//! last doctor run. Each start records systemd's `NRestarts` and how the
//! run before it ended on its `workers` row (`successor::record_start`),
//! so a crash loop is counted from the store, not the journal. A
//! successor started on a staged release is a handoff, not a restart:
//! every landed release starts one, and its parent is still draining.

use super::{Check, DAY, Status, check};
use crate::store::Store;

/// More starts than this since the last doctor run is a crash loop.
pub(super) const MAX_STARTS: usize = 3;

pub(super) fn watch(store: &Store, now: i64, last_run: Option<i64>) -> Check {
    let since = last_run.unwrap_or(now - DAY);
    let (handoffs, starts): (Vec<_>, Vec<_>) = match store.worker_starts_since(since) {
        Ok(s) => s.into_iter().partition(|s| s.successor_of.is_some()),
        Err(e) => return check("worker_restarts", Status::Warn, format!("{e:#}"), ""),
    };
    let handoffs = match handoffs.len() {
        0 => String::new(),
        n => format!(" (and {n} release handoff(s))"),
    };
    let window = match last_run {
        Some(t) => format!("since the last doctor run ({})", crate::render::utc(t)),
        None => "in the last 24h".to_string(),
    };
    let restarts = starts
        .iter()
        .rev()
        .find_map(|s| s.restarts)
        .map_or(String::new(), |n| format!("; systemd NRestarts {n}"));
    let last_exit = starts
        .iter()
        .rev()
        .map(|s| s.exit_reason.as_str())
        .find(|r| !r.is_empty())
        .unwrap_or("not recorded");
    if starts.len() > MAX_STARTS {
        check(
            "worker_restarts",
            Status::Warn,
            format!(
                "{} worker start(s){handoffs} {window}{restarts}; last exit: {last_exit}",
                starts.len()
            ),
            "the worker is restarting; read its last exit, then `journalctl --user -u forge-worker`",
        )
    } else {
        check(
            "worker_restarts",
            Status::Ok,
            format!(
                "{} worker start(s){handoffs} {window}{restarts}",
                starts.len()
            ),
            "",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, store};
    use super::*;

    fn start(s: &Store, at: i64, restarts: i64, why: &str) {
        s.lock()
            .execute(
                "INSERT INTO workers (pid, version, started_at, restarts, exit_reason)
                 VALUES (1, 'r', ?1, ?2, ?3)",
                rusqlite::params![at, restarts, why],
            )
            .unwrap();
    }

    #[test]
    fn four_starts_since_the_last_run_warn_with_the_last_exit() {
        let (_d, s) = store();
        for i in 0..4 {
            start(&s, NOW - 600 + i, 10 + i, "systemd: exit-code (status 101)");
        }
        let c = watch(&s, NOW, Some(NOW - 3600));
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("4 worker start(s)"), "{}", c.detail);
        assert!(c.detail.contains("NRestarts 13"), "{}", c.detail);
        assert!(c.detail.contains("exit-code (status 101)"), "{}", c.detail);
    }

    #[test]
    fn release_handoffs_are_not_restarts() {
        let (_d, s) = store();
        for i in 0..4 {
            start(&s, NOW - 600 + i, 21987, "release handoff from worker 7");
            s.lock()
                .execute(
                    "UPDATE workers SET successor_of = 7 WHERE id = last_insert_rowid()",
                    [],
                )
                .unwrap();
        }
        start(&s, NOW - 60, 21987, "worker 1 stopped cleanly");
        let c = watch(&s, NOW, Some(NOW - 3600));
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        assert!(c.detail.contains("1 worker start(s)"), "{}", c.detail);
        assert!(c.detail.contains("4 release handoff(s)"), "{}", c.detail);
    }

    #[test]
    fn starts_before_the_last_run_do_not_count() {
        let (_d, s) = store();
        for i in 0..6 {
            start(&s, NOW - 7200 + i, i, "");
        }
        start(&s, NOW - 60, 6, "worker 1 stopped cleanly");
        let c = watch(&s, NOW, Some(NOW - 3600));
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        assert!(c.detail.contains("1 worker start(s)"), "{}", c.detail);
    }
}
