//! Reloadable machine/project budgets, including draining workers.
use crate::{ctx::Forge, store::Store};
use anyhow::Result;
use std::collections::BTreeMap;

pub fn slots(f: &Forge, opts: &super::WorkOpts) -> usize {
    opts.jobs.unwrap_or(f.worker.slots)
}

/// Count tasks AND jobs in the same snapshot, including draining workers.
/// Orphan recovery runs before claiming; unrecovered rows conservatively hold
/// capacity. This also covers one-shot workers that do not register a release.
pub fn usage(conn: &rusqlite::Connection) -> Result<BTreeMap<String, usize>> {
    let mut stmt = conn.prepare(
        "SELECT project, COUNT(*) FROM (
        SELECT COALESCE(project, repo) AS project FROM tasks WHERE state='running'
        UNION ALL SELECT project FROM jobs WHERE state='running'
    ) GROUP BY project",
    )?;
    Ok(stmt
        .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn used(store: &Store) -> Result<BTreeMap<String, usize>> {
    usage(&store.lock())
}

/// The one-minute system load divided by online CPUs (not the current
/// process's affinity, which may be narrower than the machine).
pub fn load_per_core() -> Option<f64> {
    let mut load = 0.0;
    // SAFETY: getloadavg writes one double; sysconf takes no pointers.
    let (ok, cores) = unsafe {
        (
            libc::getloadavg(&mut load, 1),
            libc::sysconf(libc::_SC_NPROCESSORS_ONLN),
        )
    };
    (ok == 1 && cores > 0).then(|| load / cores as f64)
}

pub fn load_holds(settings: &crate::config::capacity::Settings, load: Option<f64>) -> bool {
    settings
        .max_load
        .zip(load)
        .is_some_and(|(cap, load)| load > cap)
}

#[derive(Default)]
pub struct Claims {
    load_held: bool,
    published: String,
}

pub fn refresh(f: &Forge, opts: &super::WorkOpts, state: &mut Claims) -> Result<usize> {
    let total = slots(f, opts);
    for w in f.store.live_workers(super::worker_alive)? {
        if w.pid == i64::from(std::process::id()) {
            f.store.set_worker_slots(w.id, total)?;
        }
    }
    let load = load_per_core();
    let held = load_holds(&f.worker, load);
    if held && !state.load_held {
        eprintln!(
            "worker load hold: {:.2} per core exceeds max_load {:.2}; claims paused",
            load.unwrap(),
            f.worker.max_load.unwrap()
        );
    }
    state.load_held = held;
    publish(f, total, held, state)?;
    Ok(if held { 0 } else { total })
}

/// Last accepted settings of the claiming worker, including a CLI override.
/// Doctor can still report them when a later config edit was rejected.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub pid: i64,
    pub start: String,
    pub slots: usize,
    pub settings: crate::config::capacity::Settings,
    pub build_env: BTreeMap<String, String>,
    pub load_held: bool,
}

fn publish(f: &Forge, total: usize, held: bool, state: &mut Claims) -> Result<()> {
    let pid = i64::from(std::process::id());
    if f.store
        .live_workers(super::worker_alive)?
        .last()
        .is_some_and(|w| w.pid != pid)
    {
        return Ok(());
    }
    let record = Snapshot {
        pid,
        start: crate::store::start_of(pid).unwrap_or_default(),
        slots: total,
        settings: f.worker.clone(),
        build_env: f.build_env.clone(),
        load_held: held,
    };
    let text = serde_json::to_string(&record)?;
    if state.published != text {
        let path = f.paths.home.join("worker.capacity.json");
        let temp = f.paths.home.join(format!("worker.capacity.{pid}.tmp"));
        std::fs::write(&temp, &text)?;
        std::fs::rename(temp, path)?;
        state.published = text;
    }
    Ok(())
}

pub fn snapshot(paths: &crate::ctx::Paths) -> Option<Snapshot> {
    let text = std::fs::read_to_string(paths.home.join("worker.capacity.json")).ok()?;
    let record: Snapshot = serde_json::from_str(&text).ok()?;
    super::worker_alive(record.pid, &record.start).then_some(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::capacity::{BUILD_VARIABLES, Settings, merge_env, validate_env};

    #[test]
    fn capacity_arithmetic_shares_machine_and_project_budgets() {
        let cfg = Settings {
            slots: 4,
            project_slots: Some(2),
            projects: BTreeMap::from([("forge".into(), 1)]),
            ..Default::default()
        };
        let used = BTreeMap::from([("forge".into(), 1), ("game".into(), 1)]);
        assert!(!cfg.allows(4, &used, "forge"));
        assert!(cfg.allows(4, &used, "game"));
        assert!(cfg.allows(4, &used, "other"));
        // A reload reducing total below current usage never cancels work,
        // but blocks claims until the old slots drain.
        assert!(!cfg.allows(1, &used, "other"));
        assert!(!cfg.allows(2, &used, "other"));
        assert_eq!(cfg.project_cap("game", 1), 1);
    }

    #[test]
    fn build_env_merge_is_allowlisted_and_repository_wins() {
        let operator = BTreeMap::from([
            ("CARGO_BUILD_JOBS".into(), "2".into()),
            ("RUST_TEST_THREADS".into(), "4".into()),
        ]);
        let mut repo = BTreeMap::from([("CARGO_BUILD_JOBS".into(), "1".into())]);
        assert!(validate_env(&operator).is_ok());
        let merged = merge_env(&operator, &repo);
        assert_eq!(merged["CARGO_BUILD_JOBS"], "1");
        assert_eq!(merged["RUST_TEST_THREADS"], "4");
        for key in ["TOKEN", "PATH", "LD_PRELOAD", "CARGO_HOME"] {
            repo.insert(key.into(), "secret".into());
            assert!(validate_env(&repo).is_err());
            assert!(!merge_env(&operator, &repo).contains_key(key));
        }
        for key in BUILD_VARIABLES {
            assert!(validate_env(&BTreeMap::from([(key.to_string(), "2".into())])).is_ok());
        }
    }

    #[test]
    fn load_threshold_and_configuration_validation() {
        let cfg = Settings {
            max_load: Some(1.5),
            ..Default::default()
        };
        assert!(!load_holds(&cfg, Some(1.5)));
        assert!(load_holds(&cfg, Some(1.51)));
        assert!(!load_holds(&cfg, None));
        assert!(!load_holds(&Settings::default(), Some(100.0)));
        for max_load in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                Settings {
                    max_load: Some(max_load),
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
    }
}
