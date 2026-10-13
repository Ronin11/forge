//! `[admission]` in the operator's config: the questions Jev answers when
//! a worker is about to claim a task, and the size prediction a local
//! model routes the workflow on (docs/EXECUTION.md, "Admission and the
//! judgment band"; docs/research/06-one-person-factories.md, 2.2).

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;

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
    size_provider: Option<String>,
    size_threshold: Option<f64>,
    size_workflows: Option<BTreeMap<String, String>>,
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
    /// The `jev` provider asked how large the change will be
    /// (`admission::size`); `None` predicts nothing. Meant for a local
    /// model trained on the repository's own landings
    /// (docs/research/indecis-size-2026-10-13.md), not for Jev.
    pub size_provider: Option<String>,
    /// The probability of the predicted size at or above which the task
    /// is routed by `size_workflows`; under it the size is recorded only.
    pub size_threshold: f64,
    /// Predicted size (`small`, `medium`, `large`) to the workflow a task
    /// of that size runs, when the request named none itself.
    pub size_workflows: BTreeMap<String, String>,
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
            && self.size_provider == o.size_provider
            && self.size_threshold == o.size_threshold
            && self.size_workflows == o.size_workflows
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
    let size_threshold = raw.size_threshold.unwrap_or(0.5);
    anyhow::ensure!(
        (0.0..=1.0).contains(&size_threshold),
        "admission.size_threshold: {size_threshold} is not a probability"
    );
    let size_workflows = raw.size_workflows.unwrap_or_default();
    for (level, wf) in &size_workflows {
        anyhow::ensure!(
            crate::admission::size::LEVELS.contains(&level.as_str()),
            "admission.size_workflows: {level:?} is not a size; sizes are {}",
            crate::admission::size::LEVELS.join(", ")
        );
        anyhow::ensure!(
            !wf.trim().is_empty(),
            "admission.size_workflows: {level} names no workflow"
        );
    }
    Ok(Admission {
        enabled: raw.enabled.unwrap_or(true),
        provider: raw.provider,
        threshold,
        per_day: raw.per_day.unwrap_or(10),
        exclude,
        size_provider: raw.size_provider,
        size_threshold,
        size_workflows,
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

    #[test]
    fn size_routing_is_off_by_default_and_a_map_names_only_the_three_sizes() {
        let a = Admission::default();
        assert!(a.size_provider.is_none());
        assert_eq!(a.size_threshold, 0.5);
        assert!(a.size_workflows.is_empty());
        let a = build(AdmissionRaw {
            size_provider: Some("indecis-size".into()),
            size_threshold: Some(0.7),
            size_workflows: Some(BTreeMap::from([(
                "large".to_string(),
                "planned".to_string(),
            )])),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(a.size_provider.as_deref(), Some("indecis-size"));
        assert_eq!(a.size_workflows["large"], "planned");
        let err = build(AdmissionRaw {
            size_workflows: Some(BTreeMap::from([(
                "huge".to_string(),
                "planned".to_string(),
            )])),
            ..Default::default()
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("not a size"), "{err:#}");
        let err = build(AdmissionRaw {
            size_threshold: Some(1.5),
            ..Default::default()
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("size_threshold"), "{err:#}");
    }
}
