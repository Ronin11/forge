//! The one place the CLI's machine-readable rows are shaped. `forge log`,
//! `forge requests` and `forge decisions` each have a text form and a
//! `--json` form; both render from the same struct here so the two forms
//! cannot drift apart. This module assembles documents; `render` turns
//! their raw text into the short, customer-safe lines they carry.

use crate::ctx::Forge;
use crate::workflows::Problem;
use crate::{config, plugins};
use anyhow::Result;
use serde::Serialize;

mod jobs;
mod projects;
mod stats;
mod tasks;

pub use jobs::*;
pub use projects::*;
pub use stats::*;
pub use tasks::*;

/// One row of `forge plugin list` / `forge plugin list --json`: a plugin as
/// discovered, where it came from, and whether it is enabled.
#[derive(Serialize)]
pub struct PluginRow {
    pub name: String,
    pub description: String,
    pub dir: String,
    pub source: String,
    pub capabilities: Vec<String>,
    pub restart: String,
    pub enabled: bool,
}

impl From<&plugins::Plugin> for PluginRow {
    fn from(p: &plugins::Plugin) -> Self {
        PluginRow {
            name: p.name.clone(),
            description: p.manifest.description.clone(),
            dir: p.dir.display().to_string(),
            source: p.root.display().to_string(),
            capabilities: p
                .manifest
                .capabilities
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
            restart: p.manifest.restart.as_str().to_string(),
            enabled: false,
        }
    }
}

/// One row of `forge plugin status` / `forge plugin status --json`: whether
/// a plugin is enabled and, per the supervisor's last record, whether it is
/// `running` (with `pid`/`uptime_secs`), `restarting` (with `restart_count`),
/// or `stopped` (with `last_exit`). A plugin no worker has ever supervised
/// reads as `stopped` with no `last_exit`.
#[derive(Serialize)]
pub struct PluginStatusRow {
    pub name: String,
    pub enabled: bool,
    pub state: String,
    pub pid: Option<i64>,
    pub uptime_secs: Option<i64>,
    pub restart_count: Option<u32>,
    pub last_exit: Option<String>,
}

impl PluginStatusRow {
    pub fn new(name: String, enabled: bool, run_state: &plugins::RunState) -> PluginStatusRow {
        let mut row = PluginStatusRow {
            name,
            enabled,
            state: String::new(),
            pid: None,
            uptime_secs: None,
            restart_count: None,
            last_exit: None,
        };
        match run_state {
            plugins::RunState::Running { pid, since } => {
                row.state = "running".into();
                row.pid = Some(*pid);
                row.uptime_secs = Some((crate::unix_now() - since).max(0));
            }
            plugins::RunState::Restarting { count } => {
                row.state = "restarting".into();
                row.restart_count = Some(*count);
            }
            plugins::RunState::Stopped { last_exit } => {
                row.state = "stopped".into();
                row.last_exit = last_exit.clone();
            }
        }
        row
    }
}

/// Every plugin found, in catalog order, with the store's enabled flag
/// merged in, plus the catalog's problems (a shadowed copy, a missing
/// configured root, a `plugin.toml` that failed to parse).
pub fn plugin_rows(f: &Forge) -> Result<(Vec<PluginRow>, Vec<Problem>)> {
    let home_cfg = config::load_home(&f.paths.home)?;
    let cat = plugins::load_catalog(&f.paths.home, &home_cfg.plugin_dirs);
    let enabled = f.store.enabled_plugins()?;
    let rows = cat
        .plugins
        .values()
        .map(|p| {
            let mut row = PluginRow::from(p);
            row.enabled = enabled.contains(&p.name);
            row
        })
        .collect();
    Ok((rows, cat.problems))
}
