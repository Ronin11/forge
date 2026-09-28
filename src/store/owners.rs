//! Who a `running` task or job belongs to. A bare pid is not an identity:
//! a container that restarts under the same pid, or a reboot that hands the
//! old pid to another process, leaves a dead worker's pid looking alive. The
//! owner is the pid together with the process's start time (field 22 of
//! `/proc/<pid>/stat`), or, where there is no `/proc`, a random id drawn once
//! per process. Recovery decides on the pair and writes guarded on it.

use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;

/// The worker a running row was claimed by, as recorded on the row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Owner {
    pub pid: Option<i64>,
    pub start: Option<String>,
}

impl Owner {
    /// This process, as `claim` records it.
    pub fn this_process() -> Owner {
        let pid = i64::from(std::process::id());
        Owner {
            pid: Some(pid),
            start: start_of(pid),
        }
    }
}

/// The process asking which rows are orphaned. `just_started` is true only
/// at startup: a row under the caller's own pid then belongs to a previous
/// incarnation, since nothing has been claimed yet. `start_of` reads another
/// process's start time (`start_of` here; tests substitute their own).
#[derive(Clone, Copy)]
pub struct Caller {
    pub pid: i64,
    pub just_started: bool,
    pub start_of: fn(i64) -> Option<String>,
}

impl Caller {
    pub fn this_process(just_started: bool) -> Caller {
        Caller {
            pid: i64::from(std::process::id()),
            just_started,
            start_of,
        }
    }

    /// Whether a running row owned by `owner` has lost its worker: no pid
    /// (a legacy row), a dead pid, a live pid whose start time differs from
    /// the recorded one (the pid was reused), or this process's own pid at
    /// startup or under another start time.
    pub fn is_orphan(&self, owner: &Owner, alive: impl Fn(i64) -> bool) -> bool {
        let Some(pid) = owner.pid else {
            return true;
        };
        if !alive(pid) {
            return true;
        }
        if pid == self.pid && self.just_started {
            return true;
        }
        match (&owner.start, (self.start_of)(pid)) {
            (Some(recorded), Some(current)) => *recorded != current,
            _ => false,
        }
    }
}

/// When the process `pid` started, in clock ticks since boot: field 22 of
/// `/proc/<pid>/stat`. `None` where there is no `/proc` or the process is
/// gone. The command name (field 2) may hold spaces and parentheses, so the
/// fields are counted from the last `)`.
pub fn process_start(pid: i64) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.get(stat.rfind(')')? + 1..)?;
    // `after_comm` begins at field 3.
    after_comm.split_whitespace().nth(19).map(str::to_string)
}

/// A random id drawn once per process, the identity where `/proc` gives no
/// start time.
fn random_worker_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        h.write_u128(nanos);
        format!(
            "w{:016x}{:016x}",
            h.finish(),
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        )
    })
}

/// The recorded identity of `pid`: this process's own is its start time or
/// its random id; another process's is its start time, `None` when unreadable.
pub fn start_of(pid: i64) -> Option<String> {
    if pid == i64::from(std::process::id()) {
        return Some(process_start(pid).unwrap_or_else(|| random_worker_id().to_string()));
    }
    process_start(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_start_reads_field_22_of_proc_stat() {
        if !std::path::Path::new("/proc/self/stat").exists() {
            return;
        }
        let me = i64::from(std::process::id());
        let start = process_start(me).unwrap();
        assert!(start.parse::<u64>().is_ok());
        assert_eq!(process_start(me), Some(start));
        assert_eq!(process_start(i64::from(i32::MAX)), None);
    }

    #[test]
    fn own_identity_is_stable_and_never_empty() {
        let me = i64::from(std::process::id());
        assert_eq!(start_of(me), start_of(me));
        assert!(start_of(me).is_some_and(|s| !s.is_empty()));
        assert_eq!(random_worker_id(), random_worker_id());
    }
}
