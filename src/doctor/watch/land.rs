//! Watch 9, `land_rate`: per project, how much of the finished work
//! landed over the last 7 and 30 days (the same numbers `forge stats`
//! prints), and a WARN for a project that finished work all week and
//! landed none of it. `step=integrate` rows are no measure of this: one
//! is written only when a merge's checks fail.

use super::{Check, DAY, Status, check};
use crate::store::Store;
use std::collections::BTreeMap;

/// A week with at least this many finished tasks and none landed warns.
pub(super) const MIN_FINISHED: i64 = 5;

/// Per project: `(finished, landed)` over 7 days and over 30.
pub type Rates = BTreeMap<String, ((i64, i64), (i64, i64))>;

pub fn rates(store: &Store, now: i64) -> Rates {
    let mut out = Rates::new();
    for (p, n, l) in store.land_rates(now - 7 * DAY).unwrap_or_default() {
        out.entry(p).or_default().0 = (n, l);
    }
    for (p, n, l) in store.land_rates(now - 30 * DAY).unwrap_or_default() {
        out.entry(p).or_default().1 = (n, l);
    }
    out
}

/// `landed/finished (pct)`, or `-` for nothing finished.
pub fn rate_text((n, l): (i64, i64)) -> String {
    if n == 0 {
        "-".to_string()
    } else {
        format!("{l}/{n} ({:.0}%)", l as f64 * 100.0 / n as f64)
    }
}

pub(super) fn watch(store: &Store, now: i64) -> Check {
    let rates = rates(store, now);
    let lines: Vec<String> = rates
        .iter()
        .map(|(p, (w, m))| format!("{p} 7d {} 30d {}", rate_text(*w), rate_text(*m)))
        .collect();
    let dry: Vec<&String> = rates
        .iter()
        .filter(|(_, (w, _))| w.0 >= MIN_FINISHED && w.1 == 0)
        .map(|(p, _)| p)
        .collect();
    if rates.is_empty() {
        check("land_rate", Status::Ok, "no task finished in 30 days", "")
    } else if dry.is_empty() {
        check("land_rate", Status::Ok, lines.join("; "), "")
    } else {
        check(
            "land_rate",
            Status::Warn,
            format!(
                "nothing landed in 7 days from {}: {}",
                dry.iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                lines.join("; ")
            ),
            "forge stats --project <name> shows where its tasks stop",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{store, task};
    use super::*;

    const AT: i64 = super::super::fixture::NOW;

    #[test]
    fn a_project_that_finished_five_tasks_in_a_week_and_landed_none_warns() {
        let (_d, s) = store();
        for _ in 0..5 {
            task(&s, "game", "b", "failed");
        }
        let c = watch(&s, AT);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("game 7d 0/5 (0%) 30d 0/5 (0%)"), "{}", c.detail);
    }

    #[test]
    fn one_landing_is_ok_and_is_counted() {
        let (_d, s) = store();
        for _ in 0..4 {
            task(&s, "game", "b", "failed");
        }
        let t = task(&s, "game", "b", "succeeded");
        s.lock()
            .execute("UPDATE tasks SET landed_sha='abc' WHERE id=?1", [t])
            .unwrap();
        let c = watch(&s, AT);
        assert_eq!(c.status, Status::Ok, "{}", c.detail);
        assert!(c.detail.contains("game 7d 1/5 (20%)"), "{}", c.detail);
    }
}
