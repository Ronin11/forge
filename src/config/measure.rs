//! `[measure]` in the operator's config: what a journal or a role-provider
//! experiment costs to run, and the fixed exploration draws that accumulate
//! head-to-head data without an operator routing tasks to it by hand.

use crate::agent::Provider;
use anyhow::{Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Default)]
pub(super) struct MeasureRaw {
    pub(super) journal_control: Option<f64>,
    pub(super) operator_usd_per_hour: Option<f64>,
    pub(super) attention_minutes_per_question: Option<f64>,
    /// `[measure] explore = { <role> = { provider = <name>, fraction =
    /// <0..1> } }`: see `Measure::explore`.
    #[serde(default)]
    pub(super) explore: BTreeMap<String, ExploreRoleRaw>,
}

#[derive(Deserialize)]
pub(super) struct ExploreRoleRaw {
    provider: String,
    fraction: f64,
}

/// Fixed fractions of tasks the operator assigns to a control arm so a
/// measurement accumulates on its own, without touching every task by
/// hand (see docs/LATER.md, "The journal measurement was ill-posed three
/// times").
pub struct Measure {
    /// Fraction of tasks, chosen deterministically from the task id, that
    /// run with the journal off when the request itself does not say
    /// `--journal` or `--no-journal`. `0.0` (the default) assigns none.
    pub journal_control: f64,
    /// What an hour of the operator's attention is worth, in dollars:
    /// `forge stats --questions` prices the questions a person handled
    /// with it. `None` (the default) prices nothing.
    pub operator_usd_per_hour: Option<f64>,
    /// How long handling one question takes the operator, for the same
    /// figure. Waiting is not attention; this is the reading and answering.
    pub attention_minutes_per_question: f64,
    /// Per-role exploration: each named role draws, independently and
    /// deterministically from the task id (see
    /// `queue::journal_control_draw`), into `provider` with probability
    /// `fraction`, when the task itself names no `--provider`. Generalises
    /// `journal_control`'s single fixed arm to an arbitrary provider per
    /// role, so `forge stats --by-role` accumulates real head-to-head data
    /// without an operator routing tasks to it by hand.
    pub explore: BTreeMap<String, ExploreRole>,
}

pub struct ExploreRole {
    pub provider: String,
    pub fraction: f64,
}

/// `[measure] explore`'s per-role provider and fraction, validated the same
/// way `[roles]` is: an unknown provider fails at startup, not mid-task.
pub(super) fn build_explore(
    raw: BTreeMap<String, ExploreRoleRaw>,
    providers: &BTreeMap<String, Provider>,
) -> Result<BTreeMap<String, ExploreRole>> {
    let mut explore = BTreeMap::new();
    for (role, e) in raw {
        if !providers.contains_key(&e.provider) {
            bail!(
                "measure.explore.{role}: unknown provider {:?}; see `forge providers` for what is configured",
                e.provider
            );
        }
        if !(0.0..=1.0).contains(&e.fraction) {
            bail!(
                "measure.explore.{role}: fraction must be between 0 and 1, got {}",
                e.fraction
            );
        }
        explore.insert(
            role,
            ExploreRole {
                provider: e.provider,
                fraction: e.fraction,
            },
        );
    }
    Ok(explore)
}
