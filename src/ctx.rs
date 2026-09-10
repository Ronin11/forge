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
    pub sandbox: Option<Sandbox>,
    pub report: Reporter,
}

impl Forge {
    /// `need_agent` resolves the sandbox and agent binary, which only the
    /// commands that run attempts need. `prefix` tags output with task ids.
    pub fn open(need_agent: bool, prefix: bool) -> Result<Forge> {
        let paths = Paths::resolve()?;
        let store = Store::open(&paths.home.join("forge.db"))?;
        let home = config::load_home(&paths.home)?;
        let sandbox = if need_agent {
            Sandbox::detect(&agent::agent_bin(), &home.sandbox)?
        } else {
            None
        };
        Ok(Forge {
            paths,
            store,
            budget: home.budget,
            sandbox,
            report: Reporter::new(prefix),
        })
    }

    /// For commands that already hold the paths and store (doctor).
    pub fn open_with(paths: Paths, store: Store) -> Result<Forge> {
        let budget = config::load_home(&paths.home)?.budget;
        Ok(Forge {
            paths,
            store,
            budget,
            sandbox: None,
            report: Reporter::new(false),
        })
    }

    pub fn sandboxed(&self) -> bool {
        self.sandbox.is_some()
    }
}
