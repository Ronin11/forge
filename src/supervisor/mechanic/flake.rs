//! Load flakes: a failing check whose failing tests the branch never went
//! near, and that pass again on the base alone. The base is checked in a
//! scratch directory archived from the task's own clone (no branch commits,
//! no history) — the same trick the tests contract's red-on-base check
//! already uses (`verify::red_on_base`).

use crate::checks::CheckResult;
use crate::config;
use crate::ctx::Forge;
use crate::store::Task;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Whether any changed file plausibly defines one of `failing`'s tests: its
/// path, minus extension and with `/` turned into `::`, appears in the
/// test's own name or the test's name appears in it, or its bare file stem
/// appears in the test name. Heuristic, on purpose forgiving toward
/// "touched": a false "touched" only costs a retry that would have worked
/// anyway, while a false "not touched" would blame the base for what the
/// branch broke.
pub fn touches_failing_tests(changed: &[String], failing: &[String]) -> bool {
    failing.iter().any(|test| {
        changed.iter().any(|path| {
            let no_ext = path.strip_suffix(".rs").unwrap_or(path);
            let module_path = no_ext.replace('/', "::");
            let stem = no_ext.rsplit('/').next().unwrap_or(no_ext);
            test.contains(module_path.as_str())
                || module_path.contains(test.as_str())
                || (!stem.is_empty() && test.contains(stem))
        })
    })
}

fn scratch_dir(t: &Task) -> PathBuf {
    PathBuf::from(format!("{}.mechanic-flake", t.worktree))
}

/// Re-runs `check`'s own command against `t.base_sha` alone, archived fresh
/// into a scratch directory next to the worktree (removed after, whatever
/// the outcome): whether it now exits clean, so the failure the branch just
/// hit is the base's own rather than something the branch caused.
pub async fn passes_on_base(
    f: &Forge,
    t: &Task,
    cfg: &config::Config,
    check: &CheckResult,
) -> Result<bool> {
    let Some(argv) = cfg.checks.get(&check.name) else {
        return Ok(false);
    };
    let repo = Path::new(&t.worktree);
    let scratch = scratch_dir(t);
    let _ = std::fs::remove_dir_all(&scratch);
    crate::sandbox::discard_provider_state(&scratch);
    let archived = crate::git::archive_all(repo, &t.base_sha, &scratch).await;
    let result = match archived {
        Ok(()) => {
            let timeout = Duration::from_secs(cfg.check_timeout_secs);
            let r = crate::checks::run_one(
                &check.level,
                &check.name,
                argv,
                &scratch,
                f.sandbox.as_ref(),
                timeout,
                &[],
            )
            .await;
            Ok(r.ok)
        }
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    crate::sandbox::discard_provider_state(&scratch);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_in_an_unchanged_file_is_not_touched() {
        let changed = vec!["src/agent.rs".to_string()];
        let failing = vec!["worker::window_hold_waits".to_string()];
        assert!(!touches_failing_tests(&changed, &failing));
    }

    #[test]
    fn a_test_in_a_changed_file_is_touched_by_module_path() {
        let changed = vec!["src/worker.rs".to_string()];
        let failing = vec!["worker::window_hold_waits".to_string()];
        assert!(touches_failing_tests(&changed, &failing));
    }

    #[test]
    fn a_test_named_after_a_changed_files_stem_is_touched() {
        let changed = vec!["src/store/attempts.rs".to_string()];
        let failing = vec!["attempts::a_repriced_attempt_keeps_its_row".to_string()];
        assert!(touches_failing_tests(&changed, &failing));
    }
}
