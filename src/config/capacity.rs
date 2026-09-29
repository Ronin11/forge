//! Operator capacity settings and allowlisted build tuning.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub slots: usize,
    /// Projects without an explicit cap can use this many slots. None uses
    /// the machine budget, preserving existing single-project workers.
    pub project_slots: Option<usize>,
    pub max_load: Option<f64>,
    pub projects: BTreeMap<String, usize>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            slots: 1,
            project_slots: None,
            max_load: None,
            projects: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.slots > 0, "worker.slots must be positive");
        ensure!(
            self.project_slots != Some(0),
            "worker.project_slots must be positive"
        );
        ensure!(
            self.projects.values().all(|n| *n > 0),
            "projects.<name>.slots must be positive"
        );
        ensure!(
            self.max_load.is_none_or(|n| n.is_finite() && n > 0.0),
            "worker.max_load must be finite and positive"
        );
        Ok(())
    }

    pub fn project_cap(&self, project: &str, total: usize) -> usize {
        self.projects
            .get(project)
            .copied()
            .or(self.project_slots)
            .unwrap_or(total)
            .min(total)
    }

    pub fn allows(&self, total: usize, used: &BTreeMap<String, usize>, project: &str) -> bool {
        used.values().sum::<usize>() < total
            && used.get(project).copied().unwrap_or(0) < self.project_cap(project, total)
    }
}

pub const BUILD_VARIABLES: &[&str] = &[
    "CARGO_BUILD_JOBS",
    "RUST_TEST_THREADS",
    "MAKEFLAGS",
    "NODE_OPTIONS",
    "GOMAXPROCS",
    "npm_config_jobs",
];

pub fn validate_env(env: &BTreeMap<String, String>) -> Result<()> {
    for (key, value) in env {
        ensure!(
            BUILD_VARIABLES.contains(&key.as_str()),
            "sandbox.env.{key} is not an allowed build-tuning variable"
        );
        ensure!(
            !value.contains('\0'),
            "sandbox.env.{key} contains a NUL byte"
        );
    }
    Ok(())
}

pub fn merge_env(
    operator: &BTreeMap<String, String>,
    repo: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    operator
        .iter()
        .chain(repo)
        .filter(|(k, _)| BUILD_VARIABLES.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_config_parses_projects_defaults_and_rejects_invalid_limits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[worker]\nslots = 8\nproject_slots = 2\nmax_load = 1.5\n[projects.forge]\nslots = 1\n[sandbox.env]\nCARGO_BUILD_JOBS = '2'\n").unwrap();
        let cfg = crate::config::load_home(dir.path()).unwrap();
        assert_eq!(cfg.worker.slots, 8);
        assert_eq!(cfg.worker.project_cap("forge", 8), 1);
        assert_eq!(cfg.worker.project_cap("game", 8), 2);
        assert_eq!(cfg.build_env["CARGO_BUILD_JOBS"], "2");
        for text in [
            "[worker]\nslots = 0",
            "[worker]\nproject_slots = 0",
            "[projects.forge]\nslots = 0",
            "[worker]\nmax_load = -1",
            "[sandbox.env]\nTOKEN = 'secret'",
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(crate::config::load_home(dir.path()).is_err(), "{text}");
        }
        assert!(
            validate_env(&BTreeMap::from([(
                "NODE_OPTIONS".into(),
                "bad\0value".into()
            )]))
            .is_err()
        );
    }
}
