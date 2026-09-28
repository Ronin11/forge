//! A jev provider's keys (`[providers.<name>]` with `runner = "jev"`): the
//! host it posts to, TypeSafe's endpoint and key, and Cloudflare's while
//! its AI Gateway credits last (see `agent::JevBackend`).

use super::ProviderRaw;
use crate::agent;
use anyhow::{Result, bail};

/// Fills `p`'s jev keys with their defaults and reads its `backend`. A jev
/// table written for Cloudflare alone (its `base_url` naming
/// `{account_id}`) keeps its keys as Cloudflare's; TypeSafe gets the
/// defaults. Any other runner keeps its keys and may not name a backend.
pub(super) fn settle(name: &str, jev: bool, p: &mut ProviderRaw) -> Result<agent::JevBackend> {
    if !jev {
        if p.backend.is_some() {
            bail!("providers.{name}.backend: only a jev provider has a backend");
        }
        return Ok(agent::JevBackend::Auto);
    }
    if p.cloudflare_url.is_none()
        && p.base_url
            .as_deref()
            .is_some_and(|u| u.contains("{account_id}"))
    {
        p.cloudflare_url = p.base_url.take();
        if p.cloudflare_api_key_env.is_none() {
            p.cloudflare_api_key_env = p.api_key_env.take();
        }
        if p.cloudflare_model.is_none() {
            p.cloudflare_model = p.model.take();
        }
    }
    for (key, default) in [
        (&mut p.model, agent::JEV_DEFAULT_MODEL),
        (&mut p.base_url, agent::JEV_DEFAULT_URL),
        (&mut p.api_key_env, agent::JEV_DEFAULT_KEY_ENV),
        (&mut p.account_id_env, agent::JEV_DEFAULT_ACCOUNT_ENV),
        (&mut p.cloudflare_url, agent::JEV_CLOUDFLARE_URL),
        (&mut p.cloudflare_api_key_env, agent::JEV_CLOUDFLARE_KEY_ENV),
        (&mut p.cloudflare_model, agent::JEV_CLOUDFLARE_MODEL),
    ] {
        key.get_or_insert_with(|| default.to_string());
    }
    p.price_usd_per_million_input
        .get_or_insert(agent::JEV_PRICE_INPUT_PER_MILLION);
    match &p.backend {
        Some(b) => b
            .parse()
            .map_err(|e| anyhow::anyhow!("providers.{name}.backend: {e}")),
        None => Ok(agent::JevBackend::Auto),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cloudflare_only_table_keeps_its_keys_as_cloudflares() {
        let url = "https://cf.example/accounts/{account_id}/ai/run";
        let mut p = ProviderRaw {
            base_url: Some(url.into()),
            api_key_env: Some("CF_TOKEN".into()),
            model: Some("typesafe/jev".into()),
            ..ProviderRaw::default()
        };
        assert_eq!(
            settle("jev", true, &mut p).unwrap(),
            agent::JevBackend::Auto
        );
        assert_eq!(p.cloudflare_url.as_deref(), Some(url));
        assert_eq!(p.cloudflare_api_key_env.as_deref(), Some("CF_TOKEN"));
        assert_eq!(p.cloudflare_model.as_deref(), Some("typesafe/jev"));
        assert_eq!(p.base_url.as_deref(), Some(agent::JEV_DEFAULT_URL));
        assert_eq!(p.api_key_env.as_deref(), Some("TYPESAFE_API_KEY"));
        assert_eq!(p.model.as_deref(), Some("jev-latest"));
    }

    #[test]
    fn only_a_jev_provider_names_a_backend() {
        let typesafe = || ProviderRaw {
            backend: Some("typesafe".into()),
            ..ProviderRaw::default()
        };
        let got = settle("jev", true, &mut typesafe()).unwrap();
        assert_eq!(got, agent::JevBackend::TypeSafe);
        assert!(settle("chat", false, &mut typesafe()).is_err());
    }
}
