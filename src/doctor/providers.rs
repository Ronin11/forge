//! The `providers` rows: what a configured provider needs from the
//! environment to answer at all.

use super::{Check, Status, check};
use crate::agent;

/// The `providers` row of each jev provider: the variables its key (and,
/// while Cloudflare is in use, its account id and token) are read from must
/// be set, or every judgment it is routed fails. Doctor runs in the
/// operator's shell, not the worker's, so a missing one is a warning: the
/// worker's credentials drop-in may still set it.
pub(super) fn check_jev_providers(
    home: &std::path::Path,
    providers: &std::collections::BTreeMap<String, agent::Provider>,
) -> Vec<Check> {
    let mut out = Vec::new();
    for (name, p) in providers {
        if p.runner != agent::Runner::Jev {
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
