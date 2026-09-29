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
    worktree: &Path,
    operator: &BTreeMap<String, String>,
    repo: &BTreeMap<String, String>,
) {
    ENVIRONMENTS
        .lock()
        .unwrap()
        .insert(worktree.to_path_buf(), merge_env(operator, repo));
}

pub fn worktree_env(worktree: &Path) -> BTreeMap<String, String> {
    ENVIRONMENTS
        .lock()
        .unwrap()
        .get(worktree)
        .cloned()
        .unwrap_or_default()
}
