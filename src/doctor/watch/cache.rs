//! Watch 10, `prompt_cache`: the share of each claude provider's prompt
//! tokens read from the cache over the last 24h. The prefix is pinned
//! byte for byte (`prompts::two_tasks_on_one_base_share_the_preamble_and_the_repo_pack_byte_for_byte`)
//! and cache reads are priced at a tenth of input, which makes a prefix
//! that stops caching the cheapest cost regression to miss: every attempt
//! still lands, the bill just multiplies. Only the claude runner reports
//! cache reads apart from input (codex folds them into `input_tokens`,
//! copilot's meaning is unmeasured), so only its rows are judged.

use super::{Check, DAY, Status, check};
use crate::store::{CacheUsage, Store};

/// Fewer attempts than this, or fewer prompt tokens than `MIN_TOKENS`,
/// over the day is too little to call a ratio.
const MIN_ATTEMPTS: i64 = 3;
const MIN_TOKENS: i64 = 200_000;

fn pct(n: i64, of: i64) -> i64 {
    if of == 0 { 0 } else { n * 100 / of }
}

fn line(u: &CacheUsage) -> String {
    let total = u.prompt_tokens();
    format!(
        "{} {}% of {:.1}M prompt tokens read from the cache over {} attempt(s) (written {}%, uncached {}%)",
        u.provider,
        pct(u.cache_read, total),
        total as f64 / 1e6,
        u.attempts,
        pct(u.cache_creation, total),
        pct(u.input, total),
    )
}

pub(super) fn watch(store: &Store, now: i64, warn_below: f64) -> Check {
    let rows = match store.cache_usage_since(now - DAY) {
        Ok(r) => r,
        Err(e) => return check("prompt_cache", Status::Warn, format!("{e:#}"), ""),
    };
    let claude: Vec<&CacheUsage> = rows
        .iter()
        .filter(|u| u.runner == crate::agent::Runner::ClaudeCli.as_str())
        .collect();
    let low: Vec<String> = claude
        .iter()
        .filter(|u| u.attempts >= MIN_ATTEMPTS && u.prompt_tokens() >= MIN_TOKENS)
        .filter(|u| u.hit_ratio().is_some_and(|r| r < warn_below))
        .map(|u| line(u))
        .collect();
    if !low.is_empty() {
        return check(
            "prompt_cache",
            Status::Warn,
            format!(
                "below {}% cache reads in the last 24h: {}",
                (warn_below * 100.0).round() as i64,
                low.join("; ")
            ),
            "the prompt prefix is changing between turns or sessions, or turns are slower \
             than the cache's life: compare the trace tokens (forge show) of a low attempt \
             with a high one, and check whether the repository's config or map changed \
             under running tasks",
        );
    }
    let detail = if claude.is_empty() {
        "no claude attempts with usage in the last 24h".to_string()
    } else {
        claude
            .iter()
            .map(|u| line(u))
            .collect::<Vec<_>>()
            .join("; ")
    };
    check("prompt_cache", Status::Ok, detail, "")
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, attempt_tokens, store, task};
    use super::*;

    const CLAUDE: &str = "claude-cli";

    #[test]
    fn a_provider_reading_little_from_the_cache_warns_with_its_share() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b", "landed");
        for _ in 0..3 {
            attempt_tokens(
                &s,
                t,
                "anthropic",
                CLAUDE,
                NOW - 60,
                (60_000, 30_000, 10_000),
            );
        }
        let row = watch(&s, NOW, 0.5);
        assert_eq!(row.status, Status::Warn, "{}", row.detail);
        assert!(
            row.detail
                .contains("anthropic 30% of 0.3M prompt tokens read from the cache over 3 attempt(s) (written 10%, uncached 60%)"),
            "{}",
            row.detail
        );
    }

    #[test]
    fn a_healthy_provider_is_one_ok_line_and_too_little_volume_is_not_judged() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b", "landed");
        for _ in 0..3 {
            attempt_tokens(&s, t, "anthropic", CLAUDE, NOW - 60, (5_000, 90_000, 5_000));
        }
        // Two attempts and few tokens: a terrible ratio that is not judged.
        attempt_tokens(&s, t, "spare", CLAUDE, NOW - 60, (50_000, 0, 0));
        attempt_tokens(&s, t, "spare", CLAUDE, NOW - 60, (50_000, 0, 0));
        let row = watch(&s, NOW, 0.5);
        assert_eq!(row.status, Status::Ok, "{}", row.detail);
        assert!(row.detail.starts_with("anthropic 90%"), "{}", row.detail);
        assert!(row.detail.contains("spare 0%"), "{}", row.detail);
    }

    #[test]
    fn codex_rows_and_old_attempts_are_not_judged() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b", "landed");
        for _ in 0..3 {
            attempt_tokens(&s, t, "openai", "codex-cli", NOW - 60, (300_000, 0, 0));
            attempt_tokens(&s, t, "anthropic", CLAUDE, NOW - 2 * DAY, (300_000, 0, 0));
        }
        let row = watch(&s, NOW, 0.5);
        assert_eq!(row.status, Status::Ok, "{}", row.detail);
        assert_eq!(row.detail, "no claude attempts with usage in the last 24h");
    }
}
