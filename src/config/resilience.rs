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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_defaults_and_overrides() {
        let c = build_cache(CacheRaw::default());
        assert_eq!(c.ttl_secs, 300);
        assert_eq!(c.cold_context_tokens, 60_000);
        assert_eq!(c.warn_below, 0.5);

        let c: CacheRaw =
            toml::from_str("ttl_secs = 3600\ncold_context_tokens = 80000\nwarn_below = 0.7\n")
                .unwrap();
        let c = build_cache(c);
        assert_eq!(
            (c.ttl_secs, c.cold_context_tokens, c.warn_below),
            (3600, 80_000, 0.7)
        );
    }
}
