//! What a workflow actually costs and achieves, computed from the tasks it
//! ran. Nothing here is declared: a workflow with too few runs is
//! `unknown`, and every rate carries its interval. This is the feedback
//! that makes choosing a workflow a comparison rather than a guess, and
//! the lookback that catches a version that made things worse.

use serde::Serialize;

/// Runs needed before a profile is reported as known.
pub const MIN_N: usize = 5;
/// How many recent terminal tasks a lookback considers.
pub const LOOKBACK: usize = 50;

/// One terminal task, as the profile sees it.
#[derive(Clone, Debug)]
pub struct Run {
    pub succeeded: bool,
    pub cost: f64,
    pub secs: f64,
    pub attempts: i64,
    /// The first task in this run's chain of retries: runs with the same
    /// root are one piece of work tried more than once.
    pub root: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Profile {
    pub n: usize,
    pub known: bool,
    pub succeeded: usize,
    pub rate: f64,
    /// Wilson 95% interval on the success rate.
    pub rate_lo: f64,
    pub rate_hi: f64,
    pub cost_per_task: f64,
    pub cost_per_success: Option<f64>,
    pub mean_secs: f64,
    pub mean_attempts: f64,
    /// Pieces of work (chains of retries) and how many of them ended verified.
    pub lineages: usize,
    pub lineages_verified: usize,
}

/// Wilson score interval at 95%.
pub fn wilson(k: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let z: f64 = 1.96;
    let n = n as f64;
    let p = k as f64 / n;
    let denom = 1.0 + z * z / n;
    let centre = p + z * z / (2.0 * n);
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt();
    ((centre - half) / denom, (centre + half) / denom)
}

pub fn profile(runs: &[Run]) -> Profile {
    let n = runs.len();
    let succeeded = runs.iter().filter(|r| r.succeeded).count();
    let (lo, hi) = wilson(succeeded, n);
    let cost: f64 = runs.iter().map(|r| r.cost).sum();
    let mut roots: std::collections::BTreeMap<i64, bool> = std::collections::BTreeMap::new();
    for r in runs {
        let e = roots.entry(r.root).or_insert(false);
        *e |= r.succeeded;
    }
    let lineages = roots.len();
    let lineages_verified = roots.values().filter(|v| **v).count();
    Profile {
        n,
        known: n >= MIN_N,
        succeeded,
        rate: if n > 0 {
            succeeded as f64 / n as f64
        } else {
            0.0
        },
        rate_lo: lo,
        rate_hi: hi,
        cost_per_task: if n > 0 { cost / n as f64 } else { 0.0 },
        cost_per_success: if succeeded > 0 {
            Some(cost / succeeded as f64)
        } else {
            None
        },
        mean_secs: if n > 0 {
            runs.iter().map(|r| r.secs).sum::<f64>() / n as f64
        } else {
            0.0
        },
        mean_attempts: if n > 0 {
            runs.iter().map(|r| r.attempts as f64).sum::<f64>() / n as f64
        } else {
            0.0
        },
        lineages,
        lineages_verified,
    }
}

/// Whether `current` is worse than `previous` beyond what the intervals
/// allow: the current success interval sits entirely below the previous.
pub fn regressed(current: &Profile, previous: &Profile) -> bool {
    current.known && previous.known && current.rate_hi < previous.rate_lo
}

impl Profile {
    /// One line for humans. Says unknown when it is.
    pub fn line(&self) -> String {
        if !self.known {
            return format!("unknown ({} of {} runs needed)", self.n, MIN_N);
        }
        let work = if self.lineages < self.n {
            format!(
                ", {}/{} pieces of work done",
                self.lineages_verified, self.lineages
            )
        } else {
            String::new()
        };
        format!(
            "{} run(s): verified {}/{} ({:.0}%, 95% {:.0}–{:.0}%){work}, ${:.2}/task, {}, {:.1} min, {:.1} attempts",
            self.n,
            self.succeeded,
            self.n,
            self.rate * 100.0,
            self.rate_lo * 100.0,
            self.rate_hi * 100.0,
            self.cost_per_task,
            self.cost_per_success
                .map_or("no success yet".to_string(), |c| format!("${c:.2}/success")),
            self.mean_secs / 60.0,
            self.mean_attempts
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(ok: usize, fail: usize, cost: f64) -> Vec<Run> {
        (0..ok)
            .map(|i| Run {
                succeeded: true,
                cost,
                secs: 60.0,
                attempts: 1,
                root: i as i64,
            })
            .chain((0..fail).map(|i| Run {
                succeeded: false,
                cost,
                secs: 120.0,
                attempts: 2,
                root: 1000 + i as i64,
            }))
            .collect()
    }

    #[test]
    fn retries_of_one_piece_of_work_count_once_for_whether_it_got_done() {
        // Three runs, two of them the same work retried: 1 of 3 runs verified, 1 of 2 pieces done.
        let mut r = runs(1, 2, 1.0);
        r[0].root = 7;
        r[1].root = 7;
        let p = profile(&r);
        assert_eq!((p.n, p.succeeded), (3, 1));
        assert_eq!((p.lineages, p.lineages_verified), (2, 1));
        let mut five = runs(2, 3, 1.0);
        for x in &mut five {
            x.root = 1;
        }
        let p = profile(&five);
        assert!(p.line().contains("1/1 pieces of work done"), "{}", p.line());
        let flat = profile(&runs(3, 2, 1.0));
        assert!(
            !flat.line().contains("pieces of work"),
            "no retries, nothing to add: {}",
            flat.line()
        );
    }

    #[test]
    fn wilson_is_sane() {
        let (lo, hi) = wilson(0, 0);
        assert_eq!((lo, hi), (0.0, 1.0));
        let (lo, hi) = wilson(5, 5);
        assert!(lo > 0.5 && hi > 0.99, "{lo} {hi}");
        let (lo, hi) = wilson(1, 10);
        assert!(lo < 0.05 && hi > 0.3 && hi < 0.5, "{lo} {hi}");
    }

    #[test]
    fn unknown_below_min_then_measured() {
        let p = profile(&runs(2, 1, 0.5));
        assert!(!p.known);
        assert!(p.line().starts_with("unknown (3 of 5"));
        let p = profile(&runs(4, 1, 0.5));
        assert!(p.known);
        assert_eq!(p.succeeded, 4);
        assert_eq!(p.cost_per_task, 0.5);
        assert_eq!(p.cost_per_success, Some(0.625));
        assert!(p.line().contains("verified 4/5 (80%"), "{}", p.line());
    }

    #[test]
    fn regression_needs_separated_intervals() {
        let good = profile(&runs(19, 1, 1.0));
        let bad = profile(&runs(1, 9, 1.0));
        let meh = profile(&runs(6, 4, 1.0));
        assert!(regressed(&bad, &good));
        assert!(
            !regressed(&meh, &good),
            "overlapping intervals are not a regression"
        );
        assert!(
            !regressed(&profile(&runs(0, 2, 1.0)), &good),
            "too few runs is unknown, not regressed"
        );
    }
}
