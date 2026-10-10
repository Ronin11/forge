//! Plugins belong to whichever worker is claiming. A worker that starts a
//! successor keeps its plugins until the successor has claimed (the
//! successor's pid is in `bin/successor-capable`), then stops them for the
//! successor's set to take over; a successor that exits, or never claims,
//! gives them back to the worker that still does. See docs/PLUGINS.md
//! "Supervision".

use super::{RunState, Supervisor, read_run_state};
use crate::ctx::Forge;
use crate::store::Store;
use crate::worker::{pid_alive, worker_alive};
use std::path::Path;
use std::sync::{Arc, Mutex};

const HANDOFF_PREFIX: &str = "stopped by worker for handoff to pid ";

/// Why a supervised plugin is being stopped; the words `forge plugin
/// status` shows after `stopped:`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StopReason {
    /// The worker is draining or restarting the plugin.
    #[default]
    Worker,
    /// The worker process is exiting, for the reason given (a signal, an
    /// environment fault, a newer release): `worker exiting: <reason>`.
    Exiting(String),
    /// `forge plugin disable`.
    Disabled,
    /// A successor worker has claimed and takes the plugins over.
    Handoff(i64),
}

impl StopReason {
    pub fn text(&self) -> String {
        match self {
            StopReason::Worker => "stopped by worker".to_string(),
            StopReason::Exiting(why) => format!("worker exiting: {why}"),
            StopReason::Disabled => "disabled".to_string(),
            StopReason::Handoff(pid) => format!("{HANDOFF_PREFIX}{pid}"),
        }
    }
}

/// Shared between a `Supervised` and the task that supervises it: read when
/// the task is told to stop.
pub(super) type Reason = Arc<Mutex<StopReason>>;

pub(super) fn reason_text(reason: &Reason) -> String {
    reason.lock().map(|r| r.text()).unwrap_or_default()
}

/// The successor pid a `Stopped` record's `last_exit` says the plugin was
/// handed to.
fn handoff_target(last_exit: &str) -> Option<i64> {
    last_exit.strip_prefix(HANDOFF_PREFIX)?.parse().ok()
}

/// `read_run_state`, but a `Running` pid the OS no longer has reads as
/// `stopped: supervisor gone` instead of forever `running`.
pub fn effective_run_state(home: &Path, name: &str) -> RunState {
    match read_run_state(home, name) {
        RunState::Running { pid, .. } if !pid_alive(pid) => RunState::Stopped {
            last_exit: Some("supervisor gone".to_string()),
        },
        state => state,
    }
}

impl Supervisor {
    /// Stop every plugin for the successor `pid` that has claimed.
    pub async fn hand_off(self, pid: i64) {
        if let Ok(mut r) = self.drain.lock() {
            *r = StopReason::Handoff(pid);
        }
        self.stop().await;
    }
}

/// Put the plugins where the claim is: stopped for `successor` once it has
/// claimed, running here otherwise. Called once per worker pass.
pub async fn settle(f: &Arc<Forge>, plugins: &mut Option<Supervisor>, successor: Option<i64>) {
    match (successor, plugins.take()) {
        (Some(pid), Some(p)) => {
            eprintln!("successor pid {pid} claims: stopping this worker's plugins for it");
            p.hand_off(pid).await;
        }
        (None, None) => {
            eprintln!("no successor claims: this worker restarts its plugins");
            *plugins = Some(Supervisor::start(f.clone()));
        }
        (_, current) => *plugins = current,
    }
}

/// The enabled plugins with no process while a worker claims, each with
/// its effective state. A plugin between restarts has a supervisor and is
/// not listed; one handed to a live successor is that worker's to run.
pub fn unattended(home: &Path, store: &Store) -> Vec<String> {
    let claiming = store
        .live_workers(worker_alive)
        .is_ok_and(|w| !w.is_empty());
    let Ok(enabled) = store.enabled_plugins() else {
        return Vec::new();
    };
    if !claiming {
        return Vec::new();
    }
    enabled
        .into_iter()
        .filter_map(|name| {
            let state = effective_run_state(home, &name);
            let has_process = match &state {
                RunState::Running { .. } => true,
                RunState::Restarting { .. } => true,
                RunState::Stopped { last_exit } => last_exit
                    .as_deref()
                    .and_then(handoff_target)
                    .is_some_and(pid_alive),
            };
            (!has_process).then(|| format!("{name} ({})", state.describe()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_reads_back_as_the_pid_it_names() {
        let text = StopReason::Handoff(1759019).text();
        assert_eq!(text, "stopped by worker for handoff to pid 1759019");
        assert_eq!(handoff_target(&text), Some(1759019));
        assert_eq!(handoff_target(&StopReason::Disabled.text()), None);
        assert_eq!(handoff_target(&StopReason::Worker.text()), None);
        assert_eq!(
            StopReason::Exiting("signal".into()).text(),
            "worker exiting: signal"
        );
    }

    /// A live worker claims, and an enabled plugin's record says `running`
    /// under a pid nothing holds any more (its supervisor died without
    /// marking it stopped): the row reads `stopped: supervisor gone`, never
    /// `running pid N, up ...` (REVIEW-4 E3-20(c)).
    #[test]
    fn a_dead_running_pid_reads_as_supervisor_gone_while_a_worker_claims() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store = Store::open(&home.join("forge.db")).unwrap();
        store.set_plugin_enabled("b", true, 1).unwrap();
        store
            .register_worker(std::process::id() as i64, "r1")
            .unwrap();

        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id() as i64;
        child.wait().unwrap();
        super::super::write_run_state(
            home,
            "b",
            &RunState::Running {
                pid: dead,
                since: 1,
            },
        );

        let rows = unattended(home, &store);
        assert_eq!(rows, ["b (stopped: supervisor gone)"]);
    }
}
