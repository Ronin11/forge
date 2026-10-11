//! The time scale a loaded host stretches deadlines by: one function, read
//! by the kernel (a check's timeout, `checks::run_one_capped`) and by the
//! e2e harness (`tests/e2e/support.rs` includes this file by path), so the
//! two can never disagree. A loaded host makes checks slower, never failed
//! (docs/CHECKS.md, "Timing").

/// `[checks] max_time_scale` when the repository sets none.
pub const DEFAULT_MAX_TIME_SCALE: f64 = 4.0;

/// Never-below-1 factor: `FORGE_TEST_TIME_SCALE` when set to a finite
/// number, else the 1-minute load average per CPU. Read fresh on each call,
/// so a check scales by the load at the moment it starts. Uncapped; the
/// kernel applies `[checks] max_time_scale` on top.
pub fn time_scale() -> f64 {
    let set = std::env::var("FORGE_TEST_TIME_SCALE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite());
    set.unwrap_or_else(|| load_average() / cpus()).max(1.0)
}

/// First field of /proc/loadavg: the 1-minute load average.
pub fn load_average() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
}

pub fn cpus() -> f64 {
    std::thread::available_parallelism().map_or(1, |n| n.get()) as f64
}
