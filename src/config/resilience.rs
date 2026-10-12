//! `[cache]`, `[infra]` and `[mechanic]` in the operator's config: what
//! Forge assumes about the provider's prompt cache, what makes a dead
//! attempt the host's or the provider's fault rather than the agent's,
//! and the strong lane the mechanic tries before handing a failure to the
//! operator (docs/research/06-one-person-factories.md, 2.3 and 2.4).

use serde::Deserialize;

#[derive(Deserialize, Default)]
pub(super) struct CacheRaw {
    ttl_secs: Option<i64>,
    cold_context_tokens: Option<i64>,
    warn_below: Option<f64>,
}

/// What Forge assumes about the provider's prompt cache. The claude CLI
/// reports `cache_read_input_tokens` per turn; Forge records the session
/// totals per attempt and never sees the cache itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cache {
    /// How long a cached prefix lives after its last use, in seconds (the
    /// provider's default ephemeral cache is five minutes). A session
    /// resumed later than this re-reads its whole context at the full
    /// input price on its first turn.
    pub ttl_secs: i64,
    /// On the `fresh` continuation arm, a session whose cache is forecast
    /// cold continues in a fresh session past this many tokens of context,
    /// instead of `handoff::CONTEXT_THRESHOLD_TOKENS` (the threshold for a
    /// warm cache).
    pub cold_context_tokens: i64,
    /// `forge doctor`'s `prompt_cache` row warns when, over the last 24h,
    /// a claude provider read less than this fraction of its prompt
    /// tokens from the cache.
    pub warn_below: f64,
}

impl Cache {
    /// The `warn_below` an operator config that cannot be read falls
    /// back to, so doctor's row still judges.
    pub fn default_warn_below() -> f64 {
        0.5
    }
}

pub(super) fn build_cache(raw: CacheRaw) -> Cache {
    Cache {
        ttl_secs: raw.ttl_secs.unwrap_or(300),
        cold_context_tokens: raw.cold_context_tokens.unwrap_or(60_000),
        warn_below: raw.warn_below.unwrap_or_else(Cache::default_warn_below),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct InfraRaw {
    load_per_core: Option<f64>,
    swap_gb: Option<f64>,
    router_secs: Option<i64>,
    limit: Option<u32>,
}

/// When a dead attempt is the host's or the provider's fault and does not
/// count against the task (`engine::step::infra`). The host is sampled the
/// moment the agent process ends, before any check runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Infra {
    /// A timed-out attempt whose host carried more than this one-minute
    /// load per online core when it died was starved, not stuck.
    pub load_per_core: f64,
    /// A timed-out attempt on a host swapping more than this many GiB at
    /// its death was starved, not stuck.
    pub swap_gb: f64,
    /// An attempt that ended without a result inside this many seconds
    /// with a provider or network error in its output never got to work.
    pub router_secs: i64,
    /// Consecutive infra failures one directive tolerates before the task
    /// fails, uncounted, so a dead host or provider never loops forever.
    pub limit: u32,
}

pub(super) fn build_infra(raw: InfraRaw) -> Infra {
    Infra {
        load_per_core: raw.load_per_core.unwrap_or(5.0),
        swap_gb: raw.swap_gb.unwrap_or(4.0),
        router_secs: raw.router_secs.unwrap_or(600),
        limit: raw.limit.unwrap_or(3),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct MechanicRaw {
    strong_lane: Option<bool>,
    strong_model: Option<String>,
    strong_per_day: Option<u32>,
}

/// The mechanic's strong lane: a failed task the mechanic recognizes no
/// mechanical kind for (or whose kind is spent) is retried once per
/// lineage on this model, with its branch and journal as context, before
/// it reaches the operator (docs/WORKFLOWS.md, "Mechanic").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mechanic {
    /// Whether the strong lane runs at all. `FORGE_STRONG_LANE=0` turns
    /// it off for one process, the way `FORGE_SUPERVISOR=0` does the
    /// supervisor: the e2e harness's default, since every failed run
    /// would otherwise file a retry.
    pub strong_lane: bool,
    /// The claude model name the strong-lane retry pins (`--model`); a task
    /// already on it, or on a non-claude provider, goes to the operator.
    pub strong_model: String,
    /// Strong-lane retries allowed per rolling day across every lineage:
    /// the lane costs several times a normal attempt.
    pub strong_per_day: u32,
}

pub(super) fn build_mechanic(raw: MechanicRaw) -> Mechanic {
    Mechanic {
        strong_lane: raw.strong_lane.unwrap_or(true)
            && super::env("STRONG_LANE").map(|v| v != "0").unwrap_or(true),
        strong_model: raw.strong_model.unwrap_or_else(|| "opus".into()),
        strong_per_day: raw.strong_per_day.unwrap_or(10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_defaults_and_overrides() {
        let c = build_cache(CacheRaw::default());
        assert_eq!(c.ttl_secs, 300);
        assert_eq!(c.cold_context_tokens, 60_000);
        assert_eq!(c.warn_below, 0.5);
        let i = build_infra(InfraRaw::default());
        assert_eq!(
            (i.load_per_core, i.swap_gb, i.router_secs, i.limit),
            (5.0, 4.0, 600, 3)
        );
        let m = build_mechanic(MechanicRaw::default());
        assert_eq!((m.strong_model.as_str(), m.strong_per_day), ("opus", 10));
        assert!(m.strong_lane);

        let c: CacheRaw =
            toml::from_str("ttl_secs = 3600\ncold_context_tokens = 80000\nwarn_below = 0.7\n")
                .unwrap();
        let c = build_cache(c);
        assert_eq!(
            (c.ttl_secs, c.cold_context_tokens, c.warn_below),
            (3600, 80_000, 0.7)
        );
        let i: InfraRaw =
            toml::from_str("load_per_core = 8.0\nswap_gb = 1.5\nrouter_secs = 120\nlimit = 1\n")
                .unwrap();
        let i = build_infra(i);
        assert_eq!(
            (i.load_per_core, i.swap_gb, i.router_secs, i.limit),
            (8.0, 1.5, 120, 1)
        );
        let m: MechanicRaw = toml::from_str(
            "strong_lane = false\nstrong_model = \"claude-opus-5-5\"\nstrong_per_day = 2\n",
        )
        .unwrap();
        let m = build_mechanic(m);
        assert_eq!(
            (m.strong_model.as_str(), m.strong_per_day),
            ("claude-opus-5-5", 2)
        );
        assert!(!m.strong_lane);
    }
}
