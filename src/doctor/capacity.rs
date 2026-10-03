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
    let (settings, build_env, total, held) = match worker::capacity::snapshot(paths) {
        Some(s) => (s.settings, s.build_env, s.slots, Some(s.load_held)),
        None => {
            let home = config::load_home(&paths.home)?;
            let slots = home.worker.slots;
            (home.worker, home.build_env, slots, None)
        }
    };
    let used = worker::capacity::used(store)?;
    let mut projects = used.clone();
    for name in settings.projects.keys() {
        projects.entry(name.clone()).or_default();
    }
    for p in store.list_active_projects()? {
        projects.entry(p.name).or_default();
    }
    let mut parts = vec![format!("{} of {total} slots", used.values().sum::<usize>())];
    for (name, n) in projects {
        let cap = settings.project_cap(&name, total);
        parts.push(format!("project {name}: {n} of {cap}"));
        for repo in store.project_repos(&name)? {
            if let Ok(env) = config::load_working_build_env(std::path::Path::new(&repo.repo)) {
                let env = config::capacity::merge_env(&build_env, &env);
                parts.push(format!(
                    "{name} build env ({}): {}",
                    repo.repo,
                    env_text(&env)
                ));
            }
        }
    }
    parts.extend(active_environments(paths, store)?);
    parts.push(format!("default build env: {}", env_text(&build_env)));
    let load = worker::capacity::load_per_core();
    parts.push(match (settings.max_load, load) {
        (Some(cap), Some(load)) => format!(
            "load {load:.2}/core, cap {cap:.2}: {}",
            if held.unwrap_or_else(|| worker::capacity::load_holds(&settings, Some(load))) {
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

fn active_environments(paths: &Paths, store: &Store) -> anyhow::Result<Vec<String>> {
    let conn = store.lock();
    let mut stmt = conn.prepare(
        "SELECT 'task ' || id AS label, worktree FROM tasks WHERE state='running'
        UNION ALL SELECT 'job ' || id, ?1 || '/job-' || id FROM jobs WHERE state='running'",
    )?;
    let records = stmt
        .query_map([paths.worktrees.to_string_lossy().as_ref()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(records
        .into_iter()
        .filter_map(|(label, tree)| {
            let env =
                crate::agent::build_env::recorded_env(&paths.home, std::path::Path::new(&tree))?;
            Some(format!("{label} active build env: {}", env_text(&env)))
        })
        .collect())
}

/// Disk health and sizes supplement the retained-worktree lifecycle row.
pub(super) fn disk(paths: &Paths, store: &Store) -> Vec<Check> {
    let mut rows = super::check_worktrees(store);
    let sizes = crate::disk::worktrees(&paths.worktrees);
    for row in &mut rows {
        row.detail.push_str(&format!("; {sizes}"));
    }
    rows.push(check_disk(paths));
    rows
}

fn check_disk(paths: &Paths) -> Check {
    let result = (|| -> anyhow::Result<_> {
        let settings = config::load_home(&paths.home)?.worker;
        let free = crate::disk::free_bytes(&paths.home)?;
        Ok((free, settings.min_free_gb))
    })();
    match result {
        Ok((free, min)) => check(
            "disk",
            if crate::disk::holds(free, min) {
                Status::Fail
            } else {
                Status::Ok
            },
            format!(
                "{}: {free} bytes free; minimum {min} GiB",
                paths.home.display()
            ),
            "forge gc --caches",
        ),
        Err(e) => check(
            "disk",
            Status::Fail,
            format!("{e:#}"),
            "check FORGE_HOME filesystem",
        ),
    }
}
