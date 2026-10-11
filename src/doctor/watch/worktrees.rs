//! Watch 5, `worktrees_disk`: the bytes under `FORGE_HOME/worktrees` and
//! the landed tasks whose tree is still there, sampled once a day into
//! the store; a WARN above `[worker] worktrees_warn_gb` or after three
//! days of growth in a row. Worktrees grew to fill the disk once already
//! while every row said OK.

use super::{Check, Status, check};
use crate::store::Store;
use std::path::Path;

pub(super) const SAMPLE: &str = "worktrees_bytes";
/// Day-over-day increases in a row that warn on their own.
pub(super) const GROWTH_DAYS: usize = 3;
const GIB: u64 = 1024 * 1024 * 1024;

/// Measure the worktrees directory and record today's sample, then judge.
pub(super) fn watch(store: &Store, worktrees: &Path, now: i64, warn_gb: u64) -> Check {
    let bytes = crate::disk::size(worktrees);
    let landed: Vec<i64> = store
        .landed_worktrees()
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, wt)| Path::new(wt).exists())
        .map(|(id, _)| id)
        .collect();
    let day = super::super::ymd(now);
    let _ = store.record_doctor_sample(SAMPLE, &day, bytes as i64, landed.len() as i64, now);
    judge(store, bytes, &landed, warn_gb)
}

pub(super) fn judge(store: &Store, bytes: u64, landed: &[i64], warn_gb: u64) -> Check {
    let samples = store
        .doctor_samples(SAMPLE, GROWTH_DAYS + 1)
        .unwrap_or_default();
    let growing = samples.len() == GROWTH_DAYS + 1 && samples.windows(2).all(|w| w[1].1 > w[0].1);
    let trend: Vec<String> = samples
        .iter()
        .map(|(d, v, _)| format!("{d} {}", super::super::human_bytes(*v as u64)))
        .collect();
    let detail = format!(
        "{} under worktrees; {} landed task(s) still have a tree{}; daily: {}",
        super::super::human_bytes(bytes),
        landed.len(),
        if landed.is_empty() {
            String::new()
        } else {
            format!(" ({:?})", landed)
        },
        trend.join(", ")
    );
    let over = bytes > warn_gb.saturating_mul(GIB);
    if over || growing {
        check(
            "worktrees_disk",
            Status::Warn,
            format!(
                "{}{detail}",
                match (over, growing) {
                    (true, _) => format!("above worktrees_warn_gb = {warn_gb}: "),
                    _ => format!("grew {GROWTH_DAYS} days in a row: "),
                }
            ),
            "forge gc removes the published ones; a landed task's tree is safe to remove",
        )
    } else {
        check("worktrees_disk", Status::Ok, detail, "")
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, store};
    use super::*;

    #[test]
    fn three_days_of_growth_warn() {
        let (_d, s) = store();
        for (i, v) in [10, 20, 30, 40].iter().enumerate() {
            let day = format!("2026-10-0{}", i + 1);
            s.record_doctor_sample(SAMPLE, &day, *v * GIB as i64, 0, NOW)
                .unwrap();
        }
        let c = judge(&s, 40 * GIB, &[], 200);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("grew 3 days in a row"), "{}", c.detail);
    }

    #[test]
    fn above_the_threshold_warns_and_names_landed_trees() {
        let (_d, s) = store();
        let c = judge(&s, 300 * GIB, &[7, 9], 200);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("worktrees_warn_gb = 200"), "{}", c.detail);
        assert!(c.detail.contains("2 landed task(s)"), "{}", c.detail);
    }

    #[test]
    fn a_flat_day_and_room_under_the_threshold_is_ok() {
        let (_d, s) = store();
        for (i, v) in [10, 20, 20, 30].iter().enumerate() {
            let day = format!("2026-10-0{}", i + 1);
            s.record_doctor_sample(SAMPLE, &day, *v, 0, NOW).unwrap();
        }
        assert_eq!(judge(&s, 30, &[], 200).status, Status::Ok);
    }

    #[test]
    fn watch_records_one_sample_a_day() {
        let (d, s) = store();
        let wt = d.path().join("worktrees");
        std::fs::create_dir_all(wt.join("1")).unwrap();
        std::fs::write(wt.join("1/f"), vec![0u8; 100]).unwrap();
        watch(&s, &wt, NOW, 200);
        watch(&s, &wt, NOW + 60, 200);
        let samples = s.doctor_samples(SAMPLE, 10).unwrap();
        assert_eq!(samples.len(), 1, "{samples:?}");
        assert_eq!(samples[0].0, "2026-10-10");
        assert!(samples[0].1 >= 100, "{samples:?}");
    }
}
