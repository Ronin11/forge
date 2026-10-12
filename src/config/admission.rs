//! `[admission]` in the operator's config: the two questions Jev answers
//! when a worker is about to claim a task (docs/EXECUTION.md, "Admission
//! and the judgment band"; docs/research/06-one-person-factories.md, 2.2).

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

/// Task text that never reaches Jev: money, secrets, customer-data
/// deletion. Case-insensitive; the operator's own list replaces it.
const DEFAULT_EXCLUDE: &[&str] = &[
    r"(?i)\b(secrets?|credentials?|api[ _-]?keys?|passwords?|private[ _-]?keys?|tokens?)\b",
    r"(?i)\b(billing|invoices?|refunds?|payouts?|pricing|payments?|charge[sd]?)\b",
    r"(?i)\b(delete|drop|purge|erase|wipe|truncate)\b.{0,40}\b(customers?|users?|accounts?|pii|personal data)\b",
];

#[derive(Deserialize, Default)]
pub(super) struct AdmissionRaw {
    enabled: Option<bool>,
    provider: Option<String>,
    threshold: Option<f64>,
    per_day: Option<u32>,
    exclude: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct Admission {
    /// Whether the step runs at all. It is also off when no `jev`
    /// provider is configured.
    pub enabled: bool,
    /// The `jev` provider to ask; `None` takes the first one by name.
    pub provider: Option<String>,
    /// The probability of `yes` at which a task is admitted.
    pub threshold: f64,
    /// Tasks the question may admit per rolling day, counted from the
    /// decision rows.
    pub per_day: u32,
    /// Task text matching any of these is blocked without asking Jev.
    pub exclude: Vec<Regex>,
}

impl Default for Admission {
    fn default() -> Self {
        build(AdmissionRaw::default()).expect("the default exclude patterns compile")
    }
}

impl PartialEq for Admission {
    fn eq(&self, o: &Self) -> bool {
        self.enabled == o.enabled
            && self.provider == o.provider
            && self.threshold == o.threshold
            && self.per_day == o.per_day
            && self
                .exclude
                .iter()
                .map(Regex::as_str)
                .eq(o.exclude.iter().map(Regex::as_str))
    }
}

pub(super) fn build(raw: AdmissionRaw) -> Result<Admission> {
    let patterns: Vec<String> = raw
        .exclude
        .unwrap_or_else(|| DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect());
    let exclude = patterns
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("admission.exclude: bad pattern {p:?}")))
        .collect::<Result<Vec<_>>>()?;
    let threshold = raw.threshold.unwrap_or(0.9);
    anyhow::ensure!(
        (0.0..=1.0).contains(&threshold),
        "admission.threshold: {threshold} is not a probability"
    );
    Ok(Admission {
        enabled: raw.enabled.unwrap_or(true),
        provider: raw.provider,
        threshold,
        per_day: raw.per_day.unwrap_or(10),
        exclude,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cap_at_ten_and_exclude_money_secrets_and_deletion() {
        let a = Admission::default();
        assert!(a.enabled);
        assert_eq!((a.per_day, a.threshold), (10, 0.9));
        let hit = |s: &str| a.exclude.iter().any(|r| r.is_match(s));
        assert!(hit("rotate the API key in config"));
        assert!(hit("add a refund button"));
        assert!(hit("delete all customer records older than a year"));
        assert!(!hit("rename the helper in src/util.rs"));
    }

    #[test]
    fn an_operator_list_replaces_the_defaults_and_a_bad_pattern_is_refused() {
        let a = build(AdmissionRaw {
            exclude: Some(vec!["^legal:".into()]),
            per_day: Some(2),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(a.exclude.len(), 1);
        assert_eq!(a.per_day, 2);
        let err = build(AdmissionRaw {
            exclude: Some(vec!["(".into()]),
            ..Default::default()
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("admission.exclude"), "{err:#}");
    }
}
