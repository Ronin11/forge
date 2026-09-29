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
    // Scratch worktrees are configured before they are created, and may be
    // removed and recreated during verification. Directory existence cannot
    // tell us whether an attempt still needs its trusted environment.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_build_environment_survives_pending_and_recreated_worktrees() {
        let home = tempfile::tempdir().unwrap();
        let scratch = home.path().join("pending-red-on-base");
        let other = home.path().join("other-attempt");
        std::fs::create_dir(&other).unwrap();
        let operator = BTreeMap::from([("CARGO_BUILD_JOBS".into(), "2".into())]);
        configure_env(home.path(), &scratch, &operator, &BTreeMap::new());
        for _ in 0..2 {
            assert!(!scratch.exists());
            configure_env(home.path(), &other, &BTreeMap::new(), &BTreeMap::new());
            std::fs::create_dir(&scratch).unwrap();
            let output = crate::agent::command_in(
                None,
                &scratch,
                &[
                    "sh".into(),
                    "-c".into(),
                    "printf '%s' \"$CARGO_BUILD_JOBS\"".into(),
                ],
                &[],
            )
            .output()
            .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), "2");
            std::fs::remove_dir(&scratch).unwrap();
        }
    }

    #[test]
    fn capacity_build_environment_is_per_worktree_and_reaches_agent_commands() {
        let home = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let operator = BTreeMap::from([("CARGO_BUILD_JOBS".into(), "2".into())]);
        configure_env(home.path(), first.path(), &operator, &BTreeMap::new());
        configure_env(
            home.path(),
            second.path(),
            &operator,
            &BTreeMap::from([("CARGO_BUILD_JOBS".into(), "1".into())]),
        );
        let argv = [
            "sh".into(),
            "-c".into(),
            "printf '%s' \"$CARGO_BUILD_JOBS\"".into(),
        ];
        for (tree, expected) in [(first.path(), "2"), (second.path(), "1")] {
            let output = crate::agent::command_in(None, tree, &argv, &[])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
            assert_eq!(
                recorded_env(home.path(), tree).unwrap()["CARGO_BUILD_JOBS"],
                expected
            );
        }
    }
}
