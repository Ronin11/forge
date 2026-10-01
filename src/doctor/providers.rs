//! The `providers` rows: what a configured provider needs from the
//! environment to answer at all.

use super::{Check, Status, check};
use crate::agent;
use crate::ctx::Paths;

/// Whether `name` is set in `pid`'s own process environment, checked by
/// scanning the NUL-separated entries of `/proc/<pid>/environ` for the
/// variable's name only: this never reads, returns or logs anything past
/// the `=` into a value. `None` when `/proc` cannot be read (no `/proc` at
/// all, the pid is gone, or this process cannot see into it — e.g. a
/// sandboxed doctor job in its own pid namespace).
fn proc_environ_has(pid: i64, name: &str) -> Option<bool> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(bytes.split(|&b| b == 0).any(|entry| {
        entry
            .strip_prefix(name.as_bytes())
            .is_some_and(|rest| rest.first() == Some(&b'='))
    }))
}

/// Whether `name` is declared by one of the worker unit's systemd
/// drop-ins (`crate::agent::dropin_dir`), checked by name only: each
/// `Environment=` line is split on `=` and only the key is compared, so a
/// credential's value is never read out of the file. `None` when the
/// drop-in directory holds no `.conf` file to check.
fn dropin_declares(name: &str) -> Option<bool> {
    dropin_declares_in(&agent::dropin_dir()?, name)
}

fn dropin_declares_in(dir: &std::path::Path, name: &str) -> Option<bool> {
    let mut any_conf = false;
    let mut found = false;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("conf") {
            continue;
        }
        any_conf = true;
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for raw in text.lines() {
            let Some(rest) = raw.trim().strip_prefix("Environment=") else {
                continue;
            };
            let rest = rest.trim_matches('"');
            if rest
                .split_whitespace()
                .any(|kv| kv.split('=').next() == Some(name))
            {
                found = true;
            }
        }
    }
    any_conf.then_some(found)
}

/// Whether the *worker* (not this doctor job's own process, which runs
/// with a scrubbed environment — see `job::operation_step`) will have
/// `name` set: the worker's live process environment first, by pid from
/// `worker.pid`, else what its systemd drop-ins declare. `None` when
/// neither source is reachable from inside a job, which the caller must
/// report as "can't tell", never as "not set".
fn worker_has_env_var(paths: &Paths, name: &str) -> Option<bool> {
    let live = crate::worker::worker_status(paths)
        .filter(|s| s.running)
        .and_then(|s| proc_environ_has(s.pid, name));
    live.or_else(|| dropin_declares(name))
}

/// Whether a jev provider's named environment variable is available: a
/// `secret:` reference resolves through the machine-local secret store
/// (a file, so this process's own environment is irrelevant); a plain
/// environment variable name is checked against the worker's own
/// environment, never this job's.
enum VarState {
    Set,
    Missing,
    /// Neither the worker's live environment nor its systemd drop-ins
    /// could be read from here.
    Unknown,
}

fn resolve_jev_var(paths: &Paths, secret: Option<&str>, var: &str) -> VarState {
    if let Some(s) = secret {
        return match crate::secret_store::resolve_at(&paths.home, Some(s), None) {
            Ok(_) => VarState::Set,
            Err(_) => VarState::Missing,
        };
    }
    match worker_has_env_var(paths, var) {
        Some(true) => VarState::Set,
        Some(false) => VarState::Missing,
        None => VarState::Unknown,
    }
}

/// Check the same references and store as the worker. Environment credentials
/// remain shell-dependent; secret references do not depend on the shell.
pub(super) fn check_jev_providers(
    paths: &Paths,
    providers: &std::collections::BTreeMap<String, agent::Provider>,
) -> Vec<Check> {
    let home = paths.home.as_path();
    let mut out = Vec::new();
    for (name, p) in providers {
        if p.runner != agent::Runner::Jev {
            if let Some(reference) = p.api_key.as_deref() {
                let available =
                    crate::secret_store::resolve_at(home, Some(reference), None).is_ok();
                let mut row = check(
                    "providers",
                    if available { Status::Ok } else { Status::Warn },
                    format!(
                        "{name}: {reference} {}",
                        if available {
                            "set"
                        } else {
                            "not set or unavailable"
                        }
                    ),
                    if available {
                        ""
                    } else {
                        "use forge secret set NAME"
                    },
                );
                row.provider = Some(name.clone());
                out.push(row);
            }
            continue;
        }
        let var = |v: &Option<String>, d: &str| v.clone().unwrap_or_else(|| d.to_string());
        let mut needed = Vec::new();
        let backend = agent::backend_for(p);
        if p.jev_backend != agent::JevBackend::Cloudflare {
            needed.push((
                p.api_key.as_deref(),
                var(&p.api_key_env, agent::JEV_DEFAULT_KEY_ENV),
            ));
        }
        if backend == agent::JevBackend::Cloudflare {
            needed.push((
                p.cloudflare_api_key.as_deref(),
                var(&p.cloudflare_key_env, agent::JEV_CLOUDFLARE_KEY_ENV),
            ));
            if p.cloudflare_url
                .as_deref()
                .unwrap_or(agent::JEV_CLOUDFLARE_URL)
                .contains("{account_id}")
            {
                needed.push((
                    p.account_id.as_deref(),
                    var(&p.account_id_env, agent::JEV_DEFAULT_ACCOUNT_ENV),
                ));
            }
        }
        let label = |s: Option<&str>, v: &str| s.map_or_else(|| format!("${v}"), str::to_owned);
        let states: Vec<(String, VarState)> = needed
            .iter()
            .map(|(s, v)| (label(*s, v), resolve_jev_var(paths, *s, v)))
            .collect();
        let missing: Vec<&str> = states
            .iter()
            .filter(|(_, s)| matches!(s, VarState::Missing))
            .map(|(n, _)| n.as_str())
            .collect();
        let unknown: Vec<&str> = states
            .iter()
            .filter(|(_, s)| matches!(s, VarState::Unknown))
            .map(|(n, _)| n.as_str())
            .collect();
        let host = match (p.jev_backend, backend) {
            (agent::JevBackend::Auto, agent::JevBackend::Cloudflare) => {
                "Cloudflare first, TypeSafe once its credits are spent"
            }
            (_, agent::JevBackend::Cloudflare) => "Cloudflare (forced)",
            (agent::JevBackend::TypeSafe, _) => "TypeSafe (forced)",
            _ => "TypeSafe",
        };
        let mut c = if !missing.is_empty() {
            check(
                "providers",
                Status::Warn,
                format!("{name}: jev on {host}; {} not set", missing.join(", ")),
                "set the named secret with forge secret set, or supply the configured environment variable",
            )
        } else if !unknown.is_empty() {
            check(
                "providers",
                Status::Warn,
                format!(
                    "{name}: jev on {host}; cannot tell from inside a job whether {} is set on the worker",
                    unknown.join(", ")
                ),
                "check on the machine running forge-worker: systemctl --user show forge-worker --property=Environment, or the worker's own forge doctor",
            )
        } else {
            check(
                "providers",
                Status::Ok,
                format!(
                    "{name}: jev on {host}; {} set",
                    states
                        .iter()
                        .map(|(n, _)| n.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "",
            )
        };
        c.provider = Some(name.clone());
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_home(dir.path().to_path_buf()).unwrap();
        (dir, paths)
    }

    #[test]
    fn secret_provider_checks_use_the_worker_store_and_name_missing_secrets() {
        let (home, paths) = test_paths();
        std::fs::create_dir(home.path().join("secrets")).unwrap();
        std::fs::write(home.path().join("secrets/config.toml"), "backend = 'file'").unwrap();
        let provider = agent::Provider {
            runner: agent::Runner::Jev,
            jev_backend: agent::JevBackend::TypeSafe,
            api_key: Some("secret:TYPESAFE_TEST".into()),
            ..Default::default()
        };
        let providers = [("test".into(), provider)].into();
        let missing = serde_json::to_string(&check_jev_providers(&paths, &providers)).unwrap();
        assert!(missing.contains("secret:TYPESAFE_TEST"));
        assert!(missing.contains("not set"));
        assert!(!missing.contains("TYPESAFE_API_KEY"));
        let value = "doctor-never-prints-this-value";
        crate::secret_store::Store::open(home.path())
            .unwrap()
            .set("TYPESAFE_TEST", value.into())
            .unwrap();
        let found = serde_json::to_string(&check_jev_providers(&paths, &providers)).unwrap();
        assert!(!found.contains("not set"));
        assert!(!found.contains(value));
    }

    /// A plain `api_key_env` (no `secret:` reference) with no worker
    /// pid file and no drop-in directory to read: the doctor job cannot
    /// tell from in here, so it must say so, not claim the key is unset.
    #[test]
    fn a_plain_env_var_with_no_reachable_worker_says_it_cannot_tell() {
        let (_home, paths) = test_paths();
        let provider = agent::Provider {
            runner: agent::Runner::Jev,
            jev_backend: agent::JevBackend::TypeSafe,
            api_key_env: Some("TYPESAFE_API_KEY".into()),
            ..Default::default()
        };
        let providers = [("test".into(), provider)].into();
        let report = serde_json::to_string(&check_jev_providers(&paths, &providers)).unwrap();
        assert!(report.contains("cannot tell from inside a job"), "{report}");
        assert!(!report.contains("not set"), "{report}");
    }

    /// `proc_environ_has` finds a name by scanning `/proc/self/environ`
    /// (this test process's own environment stands in for the worker's),
    /// and never matches a value that merely contains the name as a
    /// substring of something else.
    #[test]
    fn proc_environ_has_matches_the_name_exactly_and_never_a_value() {
        // `/proc/<pid>/environ` is the environment at exec, so this spawns
        // a real child with it set rather than mutating this test
        // process's own (which `/proc/self/environ` would not reflect).
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .env_clear()
            .env(
                "FORGE_DOCTOR_PROVIDER_TEST_VAR",
                "sentinel-value-FORGE_DOCTOR_PROVIDER_TEST_VAR",
            )
            .spawn()
            .unwrap();
        let pid = child.id() as i64;
        assert_eq!(
            proc_environ_has(pid, "FORGE_DOCTOR_PROVIDER_TEST_VAR"),
            Some(true)
        );
        assert_eq!(
            proc_environ_has(pid, "sentinel-value-FORGE_DOCTOR_PROVIDER_TEST_VAR"),
            Some(false)
        );
        assert_eq!(
            proc_environ_has(pid, "FORGE_DOCTOR_PROVIDER_TEST_VAR_X"),
            Some(false)
        );
        child.kill().unwrap();
        child.wait().unwrap();
    }

    /// `dropin_declares_in` finds the variable's name in an
    /// `Environment=` line without ever surfacing the value it is set to.
    #[test]
    fn dropin_declares_finds_the_name_without_reading_the_value_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("jev.conf"),
            "[Service]\nEnvironment=\"TYPESAFE_API_KEY=do-not-print-this\"\n",
        )
        .unwrap();
        assert_eq!(
            dropin_declares_in(dir.path(), "TYPESAFE_API_KEY"),
            Some(true)
        );
        assert_eq!(dropin_declares_in(dir.path(), "OTHER_VAR"), Some(false));
    }
}
