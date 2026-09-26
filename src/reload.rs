//! Config reloads between claims (docs/OPS.md, "The running binary"): a
//! running worker re-reads `FORGE_HOME/config.toml` before its next claim
//! when the file's mtime or size moved (or on SIGHUP), re-validates it
//! exactly as `forge work` does at start (`Forge::reopen`), and claims
//! with the new `Forge` from then on. An attempt in flight keeps the
//! `Arc<Forge>` it was spawned with, so its provider and prices never
//! change under it. An edit that fails validation is logged once and the
//! previous config stays in force; `worker.config.json` records what the
//! worker loaded and what it rejected, for `forge doctor`'s config row.

use crate::ctx::{Forge, Paths};
use crate::report::Event;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

/// Which version of `config.toml` a worker read: its mtime (nanoseconds
/// since the epoch) and size, so two edits within one mtime tick still
/// differ when their lengths do. `None` when there is no file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub mtime_ns: u64,
    pub len: u64,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    let mtime_ns = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    Some(Stamp {
        mtime_ns,
        len: m.len(),
    })
}

/// A config edit the worker refused, and why.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rejected {
    pub stamp: Option<Stamp>,
    pub error: String,
}

/// `FORGE_HOME/worker.config.json`: the config the worker with `pid` runs
/// on, and the last edit it rejected since, if any.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub pid: i64,
    pub loaded: Option<Stamp>,
    pub loaded_at: i64,
    pub rejected: Option<Rejected>,
}

pub fn record_path(paths: &Paths) -> PathBuf {
    paths.home.join("worker.config.json")
}

pub fn read_record(paths: &Paths) -> Option<Record> {
    let text = std::fs::read_to_string(record_path(paths)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The worker's side: what it loaded, and what it already refused (so a
/// broken file is logged once, not on every poll).
pub struct Reloader {
    record: Record,
}

impl Reloader {
    /// For a worker that just opened `f`: the file as it is now is what it
    /// runs on.
    pub fn start(f: &Forge) -> Reloader {
        let r = Reloader {
            record: Record {
                pid: std::process::id() as i64,
                loaded: stamp(&config_path(f)),
                loaded_at: crate::unix_now(),
                rejected: None,
            },
        };
        r.write(f);
        r
    }

    /// A fresh `Forge` when `config.toml` changed since the last load (or
    /// `force`, on SIGHUP) and the new file validates; `None` otherwise,
    /// with a refused edit logged the first time it is seen.
    pub fn check(&mut self, f: &Arc<Forge>, force: bool) -> Option<Arc<Forge>> {
        let path = config_path(f);
        let now = stamp(&path);
        let seen_rejected = self
            .record
            .rejected
            .as_ref()
            .is_some_and(|r| r.stamp == now);
        if !force && (now == self.record.loaded || seen_rejected) {
            return None;
        }
        match f.reopen() {
            Ok(next) => {
                self.record.loaded = now;
                self.record.loaded_at = crate::unix_now();
                self.record.rejected = None;
                let next = Arc::new(next);
                next.report.emit(
                    0,
                    Event::Note {
                        text: &format!("config: reloaded {}; new attempts use it", path.display()),
                    },
                );
                self.write(&next);
                Some(next)
            }
            Err(e) => {
                f.report.emit(
                    0,
                    Event::Note {
                        text: &format!(
                            "config: {} rejected, the previous config stays in force: {e:#}",
                            path.display()
                        ),
                    },
                );
                self.record.rejected = Some(Rejected {
                    stamp: now,
                    error: format!("{e:#}"),
                });
                self.write(f);
                None
            }
        }
    }

    fn write(&self, f: &Forge) {
        if let Ok(text) = serde_json::to_string(&self.record) {
            let _ = std::fs::write(record_path(&f.paths), text + "\n");
        }
    }
}

fn config_path(f: &Forge) -> PathBuf {
    f.paths.home.join("config.toml")
}

/// For doctor's config row: when a live worker rejected the current
/// `config.toml`, the sentence saying so, naming the file and that the
/// worker still runs the older config it loaded before it.
pub fn rejected_by_worker(paths: &Paths) -> Option<String> {
    let rec = read_record(paths)?;
    if rec.pid <= 0 || !crate::worker::pid_alive(rec.pid) {
        return None;
    }
    let path = paths.home.join("config.toml");
    let now = stamp(&path);
    let rejected = rec.rejected.filter(|r| r.stamp == now)?;
    Some(format!(
        "{} is newer than the config worker {} loaded and failed validation there ({}); the worker keeps the config it loaded",
        path.display(),
        rec.pid,
        rejected.error
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn fixture() -> (tempfile::TempDir, Arc<Forge>) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, Arc::new(f))
    }

    #[test]
    fn an_unchanged_file_reloads_nothing_and_an_edit_reloads_once() {
        let (_dir, f) = fixture();
        let path = f.paths.home.join("config.toml");
        std::fs::write(&path, "[budget]\nper_task_usd = 3.0\n").unwrap();
        let mut r = Reloader::start(&f);
        assert!(r.check(&f, false).is_none());
        std::fs::write(&path, "[budget]\nper_task_usd = 4.25\n").unwrap();
        let next = r.check(&f, false).expect("the edit reloads");
        assert_eq!(next.budget.per_task_usd, 4.25);
        // The attempt in flight still holds the old one.
        assert_eq!(f.budget.per_task_usd, 2.0);
        assert!(r.check(&next, false).is_none());
    }

    #[test]
    fn an_invalid_edit_is_refused_once_and_recorded() {
        let (_dir, f) = fixture();
        let path = f.paths.home.join("config.toml");
        let mut r = Reloader::start(&f);
        std::fs::write(&path, "[roles]\ncode = \"no-such-provider\"\n").unwrap();
        assert!(r.check(&f, false).is_none());
        let rec = read_record(&f.paths).unwrap();
        let rejected = rec.rejected.expect("recorded");
        assert!(rejected.error.contains("no-such-provider"), "{rejected:?}");
        assert_eq!(rejected.stamp, stamp(&path));
        // Seen already: not retried (nor logged) on the next poll.
        assert!(r.check(&f, false).is_none());
        // The worker is this process, alive: doctor names the file.
        let line = rejected_by_worker(&f.paths).expect("doctor line");
        assert!(line.contains("config.toml is newer"), "{line}");
        // Fixed: the next check loads it and clears the rejection.
        std::fs::write(&path, "[budget]\nper_task_usd = 1.5\n").unwrap();
        let next = r.check(&f, false).expect("fixed file reloads");
        assert_eq!(next.budget.per_task_usd, 1.5);
        assert!(read_record(&f.paths).unwrap().rejected.is_none());
        assert!(rejected_by_worker(&f.paths).is_none());
    }
}
