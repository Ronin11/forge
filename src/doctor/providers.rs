//! The `providers` rows: what a configured provider needs from the
//! environment to answer at all.

use super::{Check, Status, check};
use crate::agent;

/// Check the same references and store as the worker. Environment credentials
/// remain shell-dependent; secret references do not depend on the shell.
pub(super) fn check_jev_providers(
    home: &std::path::Path,
    providers: &std::collections::BTreeMap<String, agent::Provider>,
) -> Vec<Check> {
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
        let missing: Vec<String> = needed
            .iter()
            .filter(|(s, v)| crate::secret_store::resolve_at(home, *s, Some(v)).is_err())
            .map(|(s, v)| s.map_or_else(|| format!("${v}"), str::to_owned))
            .collect();
        let host = match (p.jev_backend, backend) {
            (agent::JevBackend::Auto, agent::JevBackend::Cloudflare) => {
                "Cloudflare first, TypeSafe once its credits are spent"
            }
            (_, agent::JevBackend::Cloudflare) => "Cloudflare (forced)",
            (agent::JevBackend::TypeSafe, _) => "TypeSafe (forced)",
            _ => "TypeSafe",
        };
        let mut c = if missing.is_empty() {
            check(
                "providers",
                Status::Ok,
                format!(
                    "{name}: jev on {host}; {} set",
                    needed
                        .iter()
                        .map(|(s, v)| s.map_or_else(|| format!("${v}"), str::to_owned))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "",
            )
        } else {
            check(
                "providers",
                Status::Warn,
                format!("{name}: jev on {host}; {} not set", missing.join(", ")),
                "set the named secret with forge secret set, or supply the configured environment variable",
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
    #[test]
    fn secret_provider_checks_use_the_worker_store_and_name_missing_secrets() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("secrets")).unwrap();
        std::fs::write(home.path().join("secrets/config.toml"), "backend = 'file'").unwrap();
        let provider = agent::Provider {
            runner: agent::Runner::Jev,
            jev_backend: agent::JevBackend::TypeSafe,
            api_key: Some("secret:TYPESAFE_TEST".into()),
            ..Default::default()
        };
        let providers = [("test".into(), provider)].into();
        let missing = serde_json::to_string(&check_jev_providers(home.path(), &providers)).unwrap();
        assert!(missing.contains("secret:TYPESAFE_TEST"));
        assert!(missing.contains("not set"));
        assert!(!missing.contains("TYPESAFE_API_KEY"));
        let value = "doctor-never-prints-this-value";
        crate::secret_store::Store::open(home.path())
            .unwrap()
            .set("TYPESAFE_TEST", value.into())
            .unwrap();
        let found = serde_json::to_string(&check_jev_providers(home.path(), &providers)).unwrap();
        assert!(!found.contains("not set"));
        assert!(!found.contains(value));
    }
}
