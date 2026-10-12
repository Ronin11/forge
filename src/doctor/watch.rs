//! The reliability watches (docs/OPS.md, "What doctor watches"). In the
//! week to 2026-10-10 five subsystems failed every time for between 2 and
//! 22 days with their evidence in the store and the event log, and every
//! doctor row said OK. Each watch here is one row, computed from the
//! store or the event log and never from the journal (it keeps seven
//! days; the supervisor's break on 09-29 was only recoverable from its
//! attempts rows), and each writes one `doctor_tripped` event to
//! `events.jsonl` the first time it goes from OK to WARN or ERROR.

use super::{Check, Status, check};
use crate::ctx::Paths;
use crate::store::{Mark, Store};
use std::path::Path;

mod cache;
mod egress;
mod environmental;
pub mod heartbeat;
mod jobs;
pub mod land;
mod restarts;
mod steps;
mod supervisor;
mod worktrees;

#[cfg(test)]
mod fixture;

const DAY: i64 = 86_400;

/// The mark that remembers when doctor last ran: `worker_restarts`
/// counts the starts since.
const LAST_RUN: &str = "doctor.last_run";

/// Every watch's row name, in the order doctor shows them.
#[cfg(test)]
const WATCHES: &[&str] = &[
    "supervisor",
    "worker_restarts",
    "steps",
    "environmental",
    "worktrees_disk",
    "plugin_heartbeat",
    "egress_model",
    "egress_refusals",
    "jobs_stale",
    "land_rate",
    "prompt_cache",
];

/// `text`'s first `n` characters on one line, with `…` when cut.
fn clip(text: &str, n: usize) -> String {
    let one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n {
        one
    } else {
        format!("{}…", one.chars().take(n).collect::<String>())
    }
}

/// Every watch's row, with any first trip written to the event log, and
/// the doctor's own last-run mark moved: `forge doctor` and the daily job.
pub(super) fn run(paths: &Paths, store: &Store, now: i64) -> Vec<Check> {
    let rows = rows(paths, store, now, false);
    let _ = store.set_doctor_mark(
        LAST_RUN,
        &Mark {
            status: "ok".into(),
            detail: String::new(),
            at: now,
        },
    );
    rows
}

/// The watches at the end of a worker's poll pass. Same rows and the same
/// first-trip events as `run`, but the last-run mark stays where the daily
/// doctor put it (`worker_restarts` counts the starts since it, which a
/// mark moved every pass would always make zero), and the worktrees
/// directory is not walked again once today's sample is in. A pass that
/// changes nothing writes nothing.
pub fn tick(paths: &Paths, store: &Store, now: i64) -> Vec<Check> {
    rows(paths, store, now, true)
}

fn rows(paths: &Paths, store: &Store, now: i64, cached: bool) -> Vec<Check> {
    let cfg = crate::config::load_home(&paths.home).ok();
    let last_run = store.doctor_mark(LAST_RUN).ok().flatten().map(|m| m.at);
    let warn_gb = cfg.as_ref().map_or_else(
        || crate::config::capacity::Settings::default().worktrees_warn_gb,
        |c| c.worker.worktrees_warn_gb,
    );
    let enabled = store.enabled_plugins().unwrap_or_default();
    let warn_below = cfg
        .as_ref()
        .map_or_else(crate::config::Cache::default_warn_below, |c| {
            c.cache.warn_below
        });
    let providers = cfg.map(|c| c.providers).unwrap_or_default();
    let mut rows = vec![
        supervisor::watch(store, now),
        restarts::watch(store, now, last_run),
        steps::watch(store),
        environmental::watch(store, now),
        if cached {
            worktrees::watch_cached(store, &paths.worktrees, now, warn_gb)
        } else {
            worktrees::watch(store, &paths.worktrees, now, warn_gb)
        },
        heartbeat::watch(&paths.home, &enabled, now),
    ];
    rows.extend(egress::watch(store, &providers, now));
    rows.push(jobs::watch(store, now));
    rows.push(land::watch(store, now));
    rows.push(cache::watch(store, now, warn_below));
    trip(store, &paths.home.join("events.jsonl"), &rows, now);
    rows
}

fn status_word(s: Status) -> &'static str {
    match s {
        Status::Ok => "ok",
        Status::Warn => "warn",
        Status::Fail => "error",
    }
}

/// Remember each row's status, and write a `doctor_tripped` event for
/// every row that was OK (or never seen) and is not now. A row that stays
/// tripped is not written again; one that clears and trips again is.
/// Returns the names written.
pub(super) fn trip(store: &Store, log: &Path, rows: &[Check], now: i64) -> Vec<String> {
    let reporter = crate::report::Reporter::new(false, Some(log.to_path_buf()));
    let mut tripped = Vec::new();
    for row in rows {
        let key = format!("watch.{}", row.name);
        let word = status_word(row.status);
        let before = store.doctor_mark(&key).ok().flatten();
        if row.status != Status::Ok && before.as_ref().is_none_or(|m| m.status == "ok") {
            reporter.record(
                0,
                &crate::report::Event::DoctorTripped {
                    check: &row.name,
                    status: word,
                    detail: &row.detail,
                    audience: "person",
                },
            );
            tripped.push(row.name.clone());
        }
        if before.as_ref().is_none_or(|m| m.status != word) {
            let _ = store.set_doctor_mark(
                &key,
                &Mark {
                    status: word.to_string(),
                    detail: row.detail.clone(),
                    at: now,
                },
            );
        }
    }
    tripped
}

#[cfg(test)]
mod tests {
    use super::fixture::{NOW, store};
    use super::*;

    #[test]
    fn a_watch_is_written_to_the_event_log_once_when_it_first_trips() {
        let (d, s) = store();
        let log = d.path().join("events.jsonl");
        let warn = || vec![check("steps", Status::Warn, "review 0 of 5", "")];
        let ok = || vec![check("steps", Status::Ok, "fine", "")];
        assert_eq!(trip(&s, &log, &ok(), NOW), Vec::<String>::new());
        assert_eq!(trip(&s, &log, &warn(), NOW), vec!["steps".to_string()]);
        assert_eq!(trip(&s, &log, &warn(), NOW), Vec::<String>::new());
        trip(&s, &log, &ok(), NOW);
        assert_eq!(trip(&s, &log, &warn(), NOW), vec!["steps".to_string()]);
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0]["type"], "doctor_tripped");
        assert_eq!(lines[0]["check"], "steps");
        assert_eq!(lines[0]["status"], "warn");
        assert_eq!(lines[0]["detail"], "review 0 of 5");
    }

    #[test]
    fn an_error_row_trips_as_error() {
        let (d, s) = store();
        let log = d.path().join("events.jsonl");
        trip(
            &s,
            &log,
            &[check("egress_model", Status::Fail, "x", "")],
            NOW,
        );
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains(r#""status":"error""#), "{text}");
    }

    #[test]
    fn run_has_a_row_for_every_watch_in_order() {
        let (d, s) = store();
        let paths = Paths {
            home: d.path().to_path_buf(),
            worktrees: d.path().join("worktrees"),
            logs: d.path().join("logs"),
        };
        let rows = run(&paths, &s, NOW);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, WATCHES);
        assert!(rows.iter().all(|r| r.status == Status::Ok), "{names:?}");
        assert_eq!(s.doctor_mark(LAST_RUN).unwrap().unwrap().at, NOW);
    }

    #[test]
    fn clip_keeps_one_line_and_marks_a_cut() {
        assert_eq!(clip("a\nb  c", 10), "a b c");
        assert_eq!(clip("abcdef", 3), "abc…");
    }
}
