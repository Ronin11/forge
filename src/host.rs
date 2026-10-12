//! What the host looked like at one moment: the one-minute load per online
//! core and the swap in use. Sampled the moment an agent process dies
//! (`attempt::run_attempt`), before any check runs and adds its own load,
//! so a timed-out attempt on a starved host can be told from one that was
//! stuck (`engine::step::infra`, docs/WORKFLOWS.md "Infra failures").

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// `worker::capacity::load_per_core`; `None` where the host cannot say.
    pub load_per_core: Option<f64>,
    /// Swap in use in GiB, from `/proc/meminfo`; `None` off Linux or
    /// without swap figures.
    pub swap_used_gb: Option<f64>,
}

impl Sample {
    /// Read the host now.
    pub fn now() -> Sample {
        Sample {
            load_per_core: crate::worker::capacity::load_per_core(),
            swap_used_gb: std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|s| parse_swap_used_gb(&s)),
        }
    }

    /// Whether the host was starved by these thresholds: load above
    /// `load_per_core` per core, or swap above `swap_gb` GiB in use.
    pub fn starved(&self, cfg: &crate::config::Infra) -> bool {
        self.load_per_core.is_some_and(|l| l > cfg.load_per_core)
            || self.swap_used_gb.is_some_and(|s| s > cfg.swap_gb)
    }

    /// The figures in a few words, for a reason line.
    pub fn describe(&self) -> String {
        let load = self
            .load_per_core
            .map_or("load unknown".to_string(), |l| format!("load {l:.1}/core"));
        let swap = self
            .swap_used_gb
            .map_or("swap unknown".to_string(), |s| format!("swap {s:.1} GiB"));
        format!("{load}, {swap}")
    }
}

/// `SwapTotal` minus `SwapFree` from a `/proc/meminfo` text, in GiB.
pub fn parse_swap_used_gb(meminfo: &str) -> Option<f64> {
    let kb = |key: &str| -> Option<f64> {
        meminfo
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
    };
    let total = kb("SwapTotal")?;
    let free = kb("SwapFree")?;
    Some(((total - free).max(0.0)) / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str =
        "MemTotal:       65536000 kB\nSwapTotal:       8388608 kB\nSwapFree:        4194304 kB\n";

    #[test]
    fn swap_in_use_is_total_minus_free_in_gib() {
        assert_eq!(parse_swap_used_gb(MEMINFO), Some(4.0));
        assert_eq!(parse_swap_used_gb("MemTotal: 1 kB\n"), None);
        assert_eq!(
            parse_swap_used_gb("SwapTotal: 0 kB\nSwapFree: 0 kB\n"),
            Some(0.0)
        );
    }

    #[test]
    fn starved_is_either_threshold_and_describe_names_both() {
        let cfg = crate::config::Infra {
            load_per_core: 5.0,
            swap_gb: 4.0,
            router_secs: 600,
            limit: 3,
        };
        let s = Sample {
            load_per_core: Some(7.26),
            swap_used_gb: Some(0.5),
        };
        assert!(s.starved(&cfg));
        assert_eq!(s.describe(), "load 7.3/core, swap 0.5 GiB");
        let s = Sample {
            load_per_core: Some(1.0),
            swap_used_gb: Some(4.5),
        };
        assert!(s.starved(&cfg));
        let s = Sample {
            load_per_core: None,
            swap_used_gb: None,
        };
        assert!(!s.starved(&cfg));
        assert_eq!(s.describe(), "load unknown, swap unknown");
    }

    #[test]
    fn the_live_sample_reads_without_panicking() {
        let s = Sample::now();
        assert!(s.load_per_core.is_none_or(|l| l >= 0.0));
        assert!(s.swap_used_gb.is_none_or(|g| g >= 0.0));
    }
}
