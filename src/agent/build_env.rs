//! Trusted build settings shared by every launch in a worktree.
use crate::config::capacity::merge_env;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

type Environments = BTreeMap<PathBuf, BTreeMap<String, String>>;
static ENVIRONMENTS: LazyLock<Mutex<Environments>> = LazyLock::new(Mutex::default);

/// Installed from the trusted config, never reread from an agent's edits.
/// Separate worktree keys keep simultaneous attempts and reloads isolated.
pub fn configure_env(
    home: &Path,
    worktree: &Path,
    operator: &BTreeMap<String, String>,
    repo: &BTreeMap<String, String>,
) {
    let env = merge_env(operator, repo);
    // A diagnostic copy outside the agent's worktree lets doctor show the
    // actual merged values of attempts surviving a config reload.
    let path = record_path(home, worktree);
    let record = || -> anyhow::Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_vec(&env)?)?;
        Ok(())
    };
    if let Err(error) = record() {
        eprintln!("build env diagnostic: {error:#}");
    }
    let mut environments = ENVIRONMENTS.lock().unwrap();
    environments.retain(|path, _| path.exists());
    environments.insert(worktree.to_path_buf(), env);
}

pub fn worktree_env(worktree: &Path) -> BTreeMap<String, String> {
    ENVIRONMENTS
        .lock()
        .unwrap()
        .get(worktree)
        .cloned()
        .unwrap_or_default()
}

fn record_path(home: &Path, worktree: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(worktree.as_os_str().as_encoded_bytes());
    home.join("cache/build-env").join(format!("{hash:x}.json"))
}

pub fn recorded_env(home: &Path, worktree: &Path) -> Option<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(record_path(home, worktree)).ok()?;
    let env = serde_json::from_str(&text).ok()?;
    crate::config::capacity::validate_env(&env).ok()?;
    Some(env)
}
