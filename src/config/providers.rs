//! `[providers.<name>]` and `[roles]` in the operator's config: the agent
//! backends beyond the built-in "anthropic" default, and which one each
//! role runs under (see `agent::Provider`).

mod jev;

use crate::agent::{Provider, Runner};
use anyhow::{Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Default)]
pub(super) struct ProviderRaw {
    runner: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    /// The environment variable that holds this provider's API key; see
    /// `agent::Provider::api_key_env`. Never the key itself.
    api_key: Option<String>,
    account_id: Option<String>,
    cloudflare_api_key: Option<String>,
    api_key_env: Option<String>,
    /// The environment variable holding the Cloudflare account id, for the
    /// jev runner; see `agent::Provider::account_id_env`.
    account_id_env: Option<String>,
    /// The jev runner's host and Cloudflare keys; see `config::providers::jev`.
    backend: Option<String>,
    cloudflare_url: Option<String>,
    cloudflare_api_key_env: Option<String>,
    cloudflare_model: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    extra_args: Vec<String>,
    notes: Option<String>,
    price_usd_per_million_input: Option<f64>,
    price_usd_per_million_output: Option<f64>,
    /// See `agent::Provider::price_cache_read_per_million`.
    price_usd_per_million_cache_read: Option<f64>,
    /// USD per premium request, for the copilot runner (see
    /// `agent::Provider::price_per_request`); default 0.
    price_usd_per_premium_request: Option<f64>,
    /// This provider's own rate-window caps; default to `[budget]`'s when
    /// absent (see `build_providers`).
    five_hour_max: Option<f64>,
    seven_day_max: Option<f64>,
    /// How many times `run_codex` may nudge a phase one that made no
    /// progress before it runs phase two; default 0 (see `agent::Provider`).
    nudges: Option<u32>,
    /// Retired (Token cost, C1): the kernel now derives every attempt's
    /// `changes[]` from git for every provider, so this key no longer
    /// changes anything. Kept, parsed and discarded so a config.toml that
    /// still sets it keeps loading instead of failing on an unknown key
    /// (see docs/CLIENT.md, "Providers").
    #[allow(dead_code)]
    report_from_git: Option<bool>,
}

/// The six roles a provider is chosen for: the four contracts, the
/// supervisor, and assess (neither is a contract; both pick a provider the
/// same way).
pub const ROLES: [&str; 6] = ["code", "tests", "review", "plan", "supervisor", "assess"];

#[derive(Deserialize, Default)]
pub(super) struct RolesRaw {
    code: Option<String>,
    tests: Option<String>,
    review: Option<String>,
    plan: Option<String>,
    supervisor: Option<String>,
    assess: Option<String>,
}

/// The built-in "anthropic" provider, plus every `[providers.<name>]` table
/// the operator declared; a table named "anthropic" overrides the built-in
/// rather than duplicating it, so an operator can, say, give it its own
/// price table without losing the runner and model every existing config
/// already relies on.
pub(super) fn build_providers(
    raw: BTreeMap<String, ProviderRaw>,
    budget: &super::Budget,
) -> Result<BTreeMap<String, Provider>> {
    let mut providers = BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        Provider {
            five_hour_max: budget.five_hour_max,
            seven_day_max: budget.seven_day_max,
            ..Provider::default()
        },
    );
    for (name, mut p) in raw {
        let runner = match &p.runner {
            Some(r) => r
                .parse::<Runner>()
                .map_err(|e| anyhow::anyhow!("providers.{name}: {e}"))?,
            None if name == "anthropic" => Runner::ClaudeCli,
            None => bail!("providers.{name}: needs a `runner`"),
        };
        for reference in [&p.api_key, &p.account_id, &p.cloudflare_api_key]
            .into_iter()
            .flatten()
        {
            crate::secret_store::reference(reference)?;
            if !matches!(runner, Runner::Chat | Runner::Jev) {
                bail!("secret references require a worker HTTP provider (chat or jev)");
            }
        }
        let jev_backend = jev::settle(&name, runner == Runner::Jev, &mut p)?;
        providers.insert(
            name.clone(),
            Provider {
                name,
                runner,
                model: p.model,
                base_url: p.base_url,
                api_key: p.api_key,
                account_id: p.account_id,
                cloudflare_api_key: p.cloudflare_api_key,
                api_key_env: p.api_key_env,
                account_id_env: p.account_id_env,
                jev_backend,
                cloudflare_url: p.cloudflare_url,
                cloudflare_key_env: p.cloudflare_api_key_env,
                cloudflare_model: p.cloudflare_model,
                env: p.env.into_iter().collect(),
                extra_args: p.extra_args,
                notes: p.notes,
                price_input_per_million: p.price_usd_per_million_input.unwrap_or(0.0),
                price_output_per_million: p.price_usd_per_million_output.unwrap_or(0.0),
                price_cache_read_per_million: p.price_usd_per_million_cache_read,
                price_per_request: p.price_usd_per_premium_request.unwrap_or(0.0),
                five_hour_max: p.five_hour_max.unwrap_or(budget.five_hour_max),
                seven_day_max: p.seven_day_max.unwrap_or(budget.seven_day_max),
                nudges: p.nudges.unwrap_or(0),
            },
        );
    }
    Ok(providers)
}

/// Every role's default provider (see `ROLES`): the operator's `[roles]`
/// table, "anthropic" where it names none. Each name must be a configured
/// provider, checked here so a typo fails at startup, not mid-task.
pub(super) fn build_roles(
    raw: RolesRaw,
    providers: &BTreeMap<String, Provider>,
) -> Result<BTreeMap<String, String>> {
    let mut roles = BTreeMap::new();
    for (role, v) in [
        ("code", raw.code),
        ("tests", raw.tests),
        ("review", raw.review),
        ("plan", raw.plan),
        ("supervisor", raw.supervisor),
        ("assess", raw.assess),
    ] {
        let name = v.unwrap_or_else(|| "anthropic".to_string());
        if !providers.contains_key(&name) {
            bail!(
                "roles.{role}: unknown provider {name:?}; see `forge providers` for what is configured"
            );
        }
        roles.insert(role.to_string(), name);
    }
    Ok(roles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_home;

    #[test]
    fn the_anthropic_provider_is_built_in() {
        let dir = tempfile::tempdir().unwrap();
        let c = load_home(dir.path()).unwrap();
        let p = &c.providers["anthropic"];
        assert_eq!(p.runner, crate::agent::Runner::ClaudeCli);
        assert_eq!(p.model.as_deref(), Some("sonnet"));
    }

    /// The two commented examples in `DEFAULT_HOME_CONFIG`, uncommented:
    /// they must parse into the fields the task said they carry.
    #[test]
    fn provider_tables_parse_runner_model_env_and_extra_args() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers.devhome]\n\
             runner = \"codex-cli\"\n\
             model = \"qwen3-coder:30b\"\n\
             env = { CODEX_OSS_BASE_URL = \"http://dev.home:11434/v1\" }\n\
             extra_args = [\"--oss\", \"--local-provider\", \"ollama\"]\n\
             \n\
             [providers.openai]\n\
             runner = \"codex-cli\"\n\
             notes = \"signed in with codex login\"\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        let devhome = &c.providers["devhome"];
        assert_eq!(devhome.runner, crate::agent::Runner::CodexCli);
        assert_eq!(devhome.model.as_deref(), Some("qwen3-coder:30b"));
        assert_eq!(
            devhome.env,
            vec![(
                "CODEX_OSS_BASE_URL".to_string(),
                "http://dev.home:11434/v1".to_string()
            )]
        );
        assert_eq!(
            devhome.extra_args,
            vec!["--oss", "--local-provider", "ollama"]
        );
        assert_eq!(devhome.price_input_per_million, 0.0);

        let openai = &c.providers["openai"];
        assert_eq!(openai.runner, crate::agent::Runner::CodexCli);
        assert_eq!(openai.model, None);
        assert!(openai.env.is_empty());
        assert_eq!(openai.notes.as_deref(), Some("signed in with codex login"));

        // The built-in default is still there alongside the operator's own.
        assert_eq!(
            c.providers["anthropic"].runner,
            crate::agent::Runner::ClaudeCli
        );
    }

    /// The commented copilot example in `DEFAULT_HOME_CONFIG`, uncommented:
    /// the runner, and the per-request price the copilot runner charges by.
    #[test]
    fn a_copilot_provider_parses_its_runner_and_per_request_price() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers.copilot]\n\
             runner = \"copilot-cli\"\n\
             price_usd_per_premium_request = 0.04\n\
             api_key_env = \"FORGE_GH_TOKEN\"\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        let p = &c.providers["copilot"];
        assert_eq!(p.runner, crate::agent::Runner::CopilotCli);
        assert_eq!(p.model, None);
        assert_eq!(p.price_per_request, 0.04);
        assert_eq!(p.price_input_per_million, 0.0);
        assert_eq!(p.api_key_env.as_deref(), Some("FORGE_GH_TOKEN"));
        // The other runners never see a per-request price.
        assert_eq!(c.providers["anthropic"].price_per_request, 0.0);
    }

    /// The two commented `runner = "chat"` examples in `DEFAULT_HOME_CONFIG`,
    /// uncommented: they must parse into the fields the task said they
    /// carry, `api_key_env` present only where the operator gave one.
    #[test]
    fn chat_provider_tables_parse_base_url_model_and_api_key_env() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers.devhome-chat]\n\
             runner = \"chat\"\n\
             base_url = \"http://dev.home:11434/v1\"\n\
             model = \"qwen3-coder:30b\"\n\
             \n\
             [providers.openai-chat]\n\
             runner = \"chat\"\n\
             base_url = \"https://api.openai.com/v1\"\n\
             model = \"gpt-5-mini\"\n\
             api_key_env = \"OPENAI_API_KEY\"\n\
             price_usd_per_million_input = 0.25\n\
             price_usd_per_million_output = 2.00\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();

        let devhome = &c.providers["devhome-chat"];
        assert_eq!(devhome.runner, crate::agent::Runner::Chat);
        assert_eq!(
            devhome.base_url.as_deref(),
            Some("http://dev.home:11434/v1")
        );
        assert_eq!(devhome.model.as_deref(), Some("qwen3-coder:30b"));
        assert_eq!(devhome.api_key_env, None);

        let openai = &c.providers["openai-chat"];
        assert_eq!(openai.runner, crate::agent::Runner::Chat);
        assert_eq!(
            openai.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(openai.model.as_deref(), Some("gpt-5-mini"));
        assert_eq!(openai.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(openai.price_input_per_million, 0.25);
        assert_eq!(openai.price_output_per_million, 2.00);
    }

    #[test]
    fn a_retired_report_from_git_key_still_parses_without_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers.devhome]\n\
             runner = \"codex-cli\"\n\
             report_from_git = true\n\
             \n\
             [providers.openai]\n\
             runner = \"codex-cli\"\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(
            c.providers["devhome"].runner,
            crate::agent::Runner::CodexCli
        );
        assert_eq!(c.providers["openai"].runner, crate::agent::Runner::CodexCli);
    }

    #[test]
    fn roles_default_to_anthropic_and_provider_caps_default_to_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let c = load_home(dir.path()).unwrap();
        for role in ROLES {
            assert_eq!(c.roles[role], "anthropic", "role {role}");
        }
        assert_eq!(c.providers["anthropic"].five_hour_max, 0.9);
        assert_eq!(c.providers["anthropic"].seven_day_max, 0.95);
    }

    #[test]
    fn roles_can_be_overridden_per_role_and_providers_can_override_their_own_caps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[budget]\nfive_hour_max = 0.8\n\
             [providers.devhome]\n\
             runner = \"codex-cli\"\n\
             five_hour_max = 0.5\n\
             \n\
             [roles]\n\
             code = \"devhome\"\n\
             review = \"devhome\"\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(c.roles["code"], "devhome");
        assert_eq!(c.roles["review"], "devhome");
        assert_eq!(c.roles["tests"], "anthropic");
        assert_eq!(c.roles["plan"], "anthropic");
        assert_eq!(c.roles["supervisor"], "anthropic");
        // Overridden explicitly.
        assert_eq!(c.providers["devhome"].five_hour_max, 0.5);
        // Not overridden: falls to the operator's own budget cap.
        assert_eq!(c.providers["devhome"].seven_day_max, 0.95);
        assert_eq!(c.providers["anthropic"].five_hour_max, 0.8);
    }

    #[test]
    fn a_role_naming_an_unconfigured_provider_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[roles]\ncode = \"does-not-exist\"\n",
        )
        .unwrap();
        let err = match load_home(dir.path()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("does-not-exist"), "{err}");
    }

    #[test]
    fn an_unknown_runner_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[providers.bogus]\nrunner = \"not-a-runner\"\n",
        )
        .unwrap();
        let err = match load_home(dir.path()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("not-a-runner"), "{err}");
    }
}
