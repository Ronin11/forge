//! `forge.toml`: the repository declares its checks; Forge runs them and
//! never trusts the agent's word for it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize, Default)]
struct Raw {
    #[serde(default)]
    checks: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    defaults: Defaults,
}

#[derive(Deserialize, Default)]
struct Defaults {
    base_branch: Option<String>,
}

pub struct Config {
    pub checks: BTreeMap<String, Vec<String>>,
    pub base_branch: String,
}

pub fn load(repo: &Path) -> Result<Config> {
    let path = repo.join("forge.toml");
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "{}: a repository must declare its checks in forge.toml",
            path.display()
        )
    })?;
    let raw: Raw = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for (name, argv) in &raw.checks {
        if argv.is_empty() {
            bail!("check `{name}` has an empty command");
        }
    }
    let base_branch = match raw.defaults.base_branch {
        Some(b) => b,
        None => crate::git::current_branch(repo)?,
    };
    Ok(Config {
        checks: raw.checks,
        base_branch,
    })
}
