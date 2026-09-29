//! The `plugins` row of `forge doctor`.

use super::{Check, Status, check};
use crate::config;
use crate::ctx::Paths;
use crate::store::Store;

/// Every plugin found across `<FORGE_HOME>/plugins` and the configured
/// `plugin_dirs`, any problem loading one (a broken `plugin.toml`, a
/// shadowed name, a missing configured root), and each enabled plugin's
/// last-known supervision state (see `crate::plugins::Supervisor`). A broken
/// or crash-looping plugin is a warning; an enabled plugin with no process
/// while a worker claims is a failure.
pub(super) fn check_plugins(paths: &Paths, store: &Store) -> Vec<Check> {
    let cfg = match config::load_home(&paths.home) {
        Ok(c) => c,
        Err(e) => return vec![check("plugins", Status::Fail, format!("{e:#}"), "")],
    };
    let cat = crate::plugins::load_catalog(&paths.home, &cfg.plugin_dirs);
    let mut detail = format!(
        "{} plugin(s) found, {} problem(s)",
        cat.plugins.len(),
        cat.problems.len()
    );
    if let Ok(enabled) = store.enabled_plugins()
        && !enabled.is_empty()
    {
        let states: Vec<String> = enabled
            .iter()
            .map(|name| {
                format!(
                    "{name} ({})",
                    crate::plugins::handoff::effective_run_state(&paths.home, name).describe()
                )
            })
            .collect();
        detail = format!("{detail}; enabled: {}", states.join(", "));
    }
    let (summary, drift_hint) = crate::plugins::drift::summarize(&cat, &paths.home);
    detail.push_str(&summary);
    let unattended = crate::plugins::handoff::unattended(&paths.home, store);
    vec![match cat.problems.first() {
        _ if !unattended.is_empty() => check(
            "plugins",
            Status::Fail,
            format!(
                "{detail}; no process while a worker claims: {}",
                unattended.join(", ")
            ),
            "the claiming worker's supervisor restarts them within a tick; if they stay down, read logs/plugins/<name>.log and `forge plugin status`",
        ),
        Some(p) => check(
            "plugins",
            Status::Warn,
            format!("{detail}: {} {}", p.file, p.what),
            "fix the plugin directory or its plugin.toml; other plugins still load",
        ),
        None if drift_hint.is_empty() => check("plugins", Status::Ok, detail, ""),
        None => check("plugins", Status::Warn, detail, drift_hint),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dead_plugin_pid_is_stopped_in_both_lists_while_a_worker_claims() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            home: dir.path().to_path_buf(),
            worktrees: dir.path().join("worktrees"),
            logs: dir.path().join("logs"),
        };
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        store.set_plugin_enabled("b", true, 1).unwrap();
        store
            .register_worker(std::process::id() as i64, "r1")
            .unwrap();

        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id() as i64;
        child.wait().unwrap();
        let run_dir = paths.home.join("plugins-run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("b.json"),
            format!(r#"{{"state":"running","pid":{dead},"since":1}}"#),
        )
        .unwrap();

        let rows = check_plugins(&paths, &store);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "plugins");
        assert_eq!(rows[0].status, Status::Fail);
        assert_eq!(
            rows[0].detail,
            "0 plugin(s) found, 0 problem(s); enabled: b (stopped: supervisor gone); \
             no process while a worker claims: b (stopped: supervisor gone)"
        );
    }
}
