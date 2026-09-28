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
