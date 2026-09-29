//! Capacity details supplement the existing worker health/handoff row.
use super::{Check, Status, check};
use crate::{config, ctx::Paths, store::Store, worker};
use std::collections::BTreeMap;

pub(super) fn describe(paths: &Paths, store: &Store, mut rows: Vec<Check>) -> Vec<Check> {
    let detail = match details(paths, store) {
        Ok(detail) => detail,
        Err(e) => {
            rows.push(check(
                "worker.capacity",
                Status::Warn,
                format!("capacity unavailable: {e:#}"),
                "check config.toml and the store",
            ));
            return rows;
        }
    };
    if rows.is_empty() {
        rows.push(check("worker", Status::Ok, "no worker registered", ""));
    }
    for row in &mut rows {
        row.detail.push_str(&format!("; {detail}"));
    }
    rows
}

fn env_text(env: &BTreeMap<String, String>) -> String {
    if env.is_empty() {
        return "none".into();
    }
    env.iter()
        .map(|(k, v)| format!("{k}={v:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn details(paths: &Paths, store: &Store) -> anyhow::Result<String> {
    let home = config::load_home(&paths.home)?;
    let live = store.live_workers(worker::worker_alive)?;
    let total = live
        .last()
        .map(|w| w.slots)
        .filter(|n| *n > 0)
        .unwrap_or(home.worker.slots);
    let used = worker::capacity::used(store)?;
    let mut projects = used.clone();
    for name in home.worker.projects.keys() {
        projects.entry(name.clone()).or_default();
    }
    for p in store.list_projects()? {
        projects.entry(p.name).or_default();
    }
    let mut parts = vec![format!("{} of {total} slots", used.values().sum::<usize>())];
    for (name, n) in projects {
        let cap = home.worker.project_cap(&name, total);
        parts.push(format!("project {name}: {n} of {cap}"));
        for repo in store.project_repos(&name)? {
            if let Ok(env) = config::load_working_build_env(std::path::Path::new(&repo.repo)) {
                let env = config::capacity::merge_env(&home.build_env, &env);
                parts.push(format!(
                    "{name} build env ({}): {}",
                    repo.repo,
                    env_text(&env)
                ));
            }
        }
    }
    parts.push(format!("default build env: {}", env_text(&home.build_env)));
    let load = worker::capacity::load_per_core();
    parts.push(match (home.worker.max_load, load) {
        (Some(cap), Some(load)) => format!(
            "load {load:.2}/core, cap {cap:.2}: {}",
            if worker::capacity::load_holds(&home.worker, Some(load)) {
                "holding claims"
            } else {
                "claims allowed"
            }
        ),
        (Some(cap), None) => format!("load unavailable, cap {cap:.2}: claims allowed"),
        (None, _) => "load cap disabled".into(),
    });
    Ok(parts.join("; "))
}
