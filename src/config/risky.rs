//! `[risky]` in the operator's config: the verdict a change to a
//! repository's risky paths waits for before it lands (`src/risky.rs`;
//! docs/WORKFLOWS.md, "Risky paths"; docs/research/06-one-person-factories.md,
//! 2.5). The paths themselves are the repository's, in its `[verify]`.

use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub(super) struct RiskyRaw {
    enabled: Option<bool>,
    provider: Option<String>,
    threshold: Option<f64>,
}

/// How a risky change is judged. The hold itself needs no provider: with
/// none, or the step off, every risky change waits for the operator.
#[derive(Clone, Debug, PartialEq)]
pub struct Risky {
    /// Whether Jev is asked at all; off, every risky change holds for the
    /// operator.
    pub enabled: bool,
    /// The `jev` provider to ask; `None` takes the first one by name.
    pub provider: Option<String>,
    /// The probability of `approve` at which the change lands.
    pub threshold: f64,
}

impl Default for Risky {
    fn default() -> Self {
        build(RiskyRaw::default()).expect("the defaults are a probability")
    }
}

pub(super) fn build(raw: RiskyRaw) -> Result<Risky> {
    let threshold = raw.threshold.unwrap_or(0.9);
    anyhow::ensure!(
        (0.0..=1.0).contains(&threshold),
        "risky.threshold: {threshold} is not a probability"
    );
    Ok(Risky {
        enabled: raw.enabled.unwrap_or(true),
        provider: raw.provider,
        threshold,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_ask_the_first_jev_at_point_nine_and_a_bad_threshold_is_refused() {
        let r = Risky::default();
        assert!(r.enabled && r.provider.is_none());
        assert_eq!(r.threshold, 0.9);
        let r = build(RiskyRaw {
            enabled: Some(false),
            provider: Some("jev".into()),
            threshold: Some(0.75),
        })
        .unwrap();
        assert!(!r.enabled);
        assert_eq!((r.provider.as_deref(), r.threshold), (Some("jev"), 0.75));
        let err = build(RiskyRaw {
            threshold: Some(1.5),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("not a probability"), "{err}");
    }
}
