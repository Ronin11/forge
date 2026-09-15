//! The process-wide context: where things live, the store, the operator's
//! budget, the sandbox, and the reporter. Built once and shared.

use crate::agent;
use crate::config::{self, Budget};
use crate::report::Reporter;
use crate::sandbox::Sandbox;
use crate::store::Store;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct Paths {
    pub home: PathBuf,
    pub worktrees: PathBuf,
    pub logs: PathBuf,
}

impl Paths {
    /// FORGE2_HOME, else $XDG_DATA_HOME/forge2, else ~/.local/share/forge2.
    /// Separate from Forge 1's FORGE_HOME so the two never share state.
    pub fn resolve() -> Result<Paths> {
        let home = if let Ok(p) = std::env::var("FORGE2_HOME") {
            PathBuf::from(p)
        } else if let Ok(p) = std::env::var("XDG_DATA_HOME") {
            PathBuf::from(p).join("forge2")
        } else {
            PathBuf::from(std::env::var("HOME").context("HOME is not set")?)
                .join(".local/share/forge2")
        };
        let p = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&p.worktrees)?;
        std::fs::create_dir_all(&p.logs)?;
        Ok(p)
    }
}

pub struct Forge {
    pub paths: Paths,
    pub store: Store,
    pub budget: Budget,
    pub supervisor: config::Supervisor,
    pub early_ending: config::EarlyEnding,
    pub measure: config::Measure,
    pub sandbox: Option<Sandbox>,
    pub report: Reporter,
}

impl Forge {
    /// `need_agent` resolves the sandbox and agent binary, which only the
    /// commands that run attempts need. `prefix` tags output with task ids.
    pub fn open(need_agent: bool, prefix: bool) -> Result<Forge> {
        let paths = Paths::resolve()?;
        let store = Store::open(&paths.home.join("forge.db"))?;
        config::ensure_home_config(&paths.home)?;
        let home = config::load_home(&paths.home)?;
        let sandbox = if need_agent {
            // Forge's own tools (forge-repomap) live beside the binary.
            let mut extra_ro = Vec::new();
            if let Ok(exe) = std::env::current_exe()
                && let Some(dir) = exe.parent()
            {
                extra_ro.push(dir.to_path_buf());
            }
            // The repository map's parsed blobs are cached here and
            // written from inside the sandbox.
            let cache = paths.home.join("cache");
            let _ = std::fs::create_dir_all(&cache);
            Sandbox::detect(&agent::agent_bin(), &home.sandbox, extra_ro, vec![cache])?
        } else {
            None
        };
        let report = Reporter::new(prefix, Some(paths.home.join("events.jsonl")));
        Ok(Forge {
            paths,
            store,
            budget: home.budget,
            supervisor: home.supervisor,
            early_ending: home.early_ending,
            measure: home.measure,
            sandbox,
            report,
        })
    }

    /// For commands that already hold the paths and store (doctor).
    pub fn open_with(paths: Paths, store: Store) -> Result<Forge> {
        let home = config::load_home(&paths.home)?;
        let report = Reporter::new(false, Some(paths.home.join("events.jsonl")));
        Ok(Forge {
            paths,
            store,
            budget: home.budget,
            supervisor: home.supervisor,
            early_ending: home.early_ending,
            measure: home.measure,
            sandbox: None,
            report,
        })
    }

    pub fn sandboxed(&self) -> bool {
        self.sandbox.is_some()
    }
}
