//! `[trust.<level>]` in the operator's config: the policy each of the three
//! trust levels a task can carry (`store::Trust`) is judged against at
//! enqueue (docs/GTM.md item 1, `queue::apply_trust_policy`).

use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub(super) struct TrustRaw {
    #[serde(default)]
    operator: TrustLevelRaw,
    #[serde(default)]
    contact: TrustLevelRaw,
    #[serde(default)]
    public: TrustLevelRaw,
}

#[derive(Deserialize, Default)]
struct TrustLevelRaw {
    per_task_usd: Option<f64>,
    per_initiative_usd: Option<f64>,
    /// `per_task_usd`'s old name; it wins when both are set.
    budget_usd: Option<f64>,
    workflows: Option<Vec<String>>,
    allow_protected: Option<bool>,
    egress: Option<String>,
    per_day: Option<u32>,
    auto_land: Option<bool>,
    allow_unsandboxed: Option<bool>,
}

/// An attempt's network policy at one trust level (docs/ROADMAP.md item 4):
/// `Model` reaches only the configured providers' model endpoints;
/// `Declared` also the hosts the repository's `[sandbox] egress` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustEgress {
    Model,
    Declared,
}

impl TrustEgress {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustEgress::Model => "model",
            TrustEgress::Declared => "declared",
        }
    }
}

/// One trust level's policy: what a task queued at that level may do,
/// enforced at enqueue by `queue::apply_trust_policy`. See `TrustPolicies`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrustPolicy {
    /// Cost cap for a task at this level; `None` is the operator's own
    /// `[budget] per_task_usd` (no tighter cap at this level). A task
    /// filed here gets it unless `--budget` says less.
    pub per_task_usd: Option<f64>,
    /// What an initiative's tasks at this level may have cost together;
    /// `None` is no cap at this level.
    pub per_initiative_usd: Option<f64>,
    /// Workflow names a task at this level may run under; `None` is every
    /// workflow.
    pub workflows: Option<Vec<String>>,
    /// Whether a task at this level may change the repository's protected
    /// paths.
    pub allow_protected: bool,
    pub egress: TrustEgress,
    /// How many tasks may start at this level per day; `None` is no cap.
    pub per_day: Option<u32>,
    /// Whether a task at this level may land itself once verified.
    pub auto_land: bool,
    /// Opts this level out of `ctx::egress_gate`.
    pub allow_unsandboxed: bool,
}

/// `[trust.<level>]` in full: the policy for each of the three levels a
/// task can carry.
#[derive(Clone, Debug, PartialEq)]
pub struct TrustPolicies {
    pub operator: TrustPolicy,
    pub contact: TrustPolicy,
    pub public: TrustPolicy,
}

/// A level's `per_task_usd`, or its old name `budget_usd`, else `default`.
fn per_task_of(l: &TrustLevelRaw, default: Option<f64>) -> Option<f64> {
    l.per_task_usd.or(l.budget_usd).or(default)
}

fn parse_trust_egress(
    level: &str,
    raw: Option<String>,
    default: TrustEgress,
) -> Result<TrustEgress> {
    match raw.as_deref() {
        None => Ok(default),
        Some("model") => Ok(TrustEgress::Model),
        Some("declared") => Ok(TrustEgress::Declared),
        Some(other) => {
            bail!("trust.{level}.egress: must be \"model\" or \"declared\", got {other:?}")
        }
    }
}

/// The three trust levels' policy: the operator's own `[trust.<level>]`
/// tables, defaulted per level (see `DEFAULT_HOME_CONFIG`'s own
/// `[trust.*]` comments, which this must agree with) when a field or a
/// whole table is absent. `operator` is unrestricted by default; `contact`
/// requires a reviewed-or-stricter workflow for its own requested work,
/// plus the front door itself (`concierge`, the decision `forge ask`
/// runs to sort a message, and `intake`, the interview a `need` decision
/// files — neither ever writes code, and without both a contact could not
/// reach `forge ask` at all), and may not touch protected paths; `public`
/// is tighter on every field, with `auto_land = false` and `per_day = 5`.
/// The cost caps default by level: public $5 a task and $25 an initiative,
/// contact $10 and $50, the operator none beyond `[budget]`.
pub(super) fn build_trust(raw: TrustRaw) -> Result<TrustPolicies> {
    Ok(TrustPolicies {
        operator: TrustPolicy {
            per_task_usd: per_task_of(&raw.operator, None),
            per_initiative_usd: raw.operator.per_initiative_usd,
            workflows: raw.operator.workflows,
            allow_protected: raw.operator.allow_protected.unwrap_or(true),
            egress: parse_trust_egress("operator", raw.operator.egress, TrustEgress::Declared)?,
            per_day: raw.operator.per_day,
            auto_land: raw.operator.auto_land.unwrap_or(true),
            allow_unsandboxed: raw.operator.allow_unsandboxed.unwrap_or(false),
        },
        contact: TrustPolicy {
            per_task_usd: per_task_of(&raw.contact, Some(10.0)),
            per_initiative_usd: Some(raw.contact.per_initiative_usd.unwrap_or(50.0)),
            workflows: raw.contact.workflows.or_else(|| {
                Some(vec![
                    "reviewed".to_string(),
                    "tdd-reviewed".to_string(),
                    "concierge".to_string(),
                    "intake".to_string(),
                ])
            }),
            allow_protected: raw.contact.allow_protected.unwrap_or(false),
            egress: parse_trust_egress("contact", raw.contact.egress, TrustEgress::Declared)?,
            per_day: raw.contact.per_day,
            auto_land: raw.contact.auto_land.unwrap_or(true),
            allow_unsandboxed: raw.contact.allow_unsandboxed.unwrap_or(false),
        },
        public: TrustPolicy {
            per_task_usd: per_task_of(&raw.public, Some(5.0)),
            per_initiative_usd: Some(raw.public.per_initiative_usd.unwrap_or(25.0)),
            workflows: raw
                .public
                .workflows
                .or_else(|| Some(vec!["reviewed".to_string()])),
            allow_protected: raw.public.allow_protected.unwrap_or(false),
            egress: parse_trust_egress("public", raw.public.egress, TrustEgress::Model)?,
            per_day: Some(raw.public.per_day.unwrap_or(5)),
            auto_land: raw.public.auto_land.unwrap_or(false),
            allow_unsandboxed: raw.public.allow_unsandboxed.unwrap_or(false),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_home;

    #[test]
    fn trust_defaults_are_operator_unrestricted_contact_reviewed_and_public_tightest() {
        let dir = tempfile::tempdir().unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(
            c.trust.operator,
            TrustPolicy {
                per_task_usd: None,
                per_initiative_usd: None,
                workflows: None,
                allow_protected: true,
                egress: TrustEgress::Declared,
                per_day: None,
                auto_land: true,
                allow_unsandboxed: false,
            }
        );
        assert_eq!(
            c.trust.contact,
            TrustPolicy {
                per_task_usd: Some(10.0),
                per_initiative_usd: Some(50.0),
                workflows: Some(vec![
                    "reviewed".to_string(),
                    "tdd-reviewed".to_string(),
                    "concierge".to_string(),
                    "intake".to_string(),
                ]),
                allow_protected: false,
                egress: TrustEgress::Declared,
                per_day: None,
                auto_land: true,
                allow_unsandboxed: false,
            }
        );
        assert_eq!(
            c.trust.public,
            TrustPolicy {
                per_task_usd: Some(5.0),
                per_initiative_usd: Some(25.0),
                workflows: Some(vec!["reviewed".to_string()]),
                allow_protected: false,
                egress: TrustEgress::Model,
                per_day: Some(5),
                auto_land: false,
                allow_unsandboxed: false,
            }
        );
    }

    #[test]
    fn trust_levels_can_be_overridden_from_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[trust.public]\n\
             per_task_usd = 0.5\n\
             per_initiative_usd = 3.0\n\
             workflows = [\"direct\"]\n\
             allow_protected = true\n\
             egress = \"declared\"\n\
             per_day = 10\n\
             auto_land = true\n",
        )
        .unwrap();
        let c = load_home(dir.path()).unwrap();
        assert_eq!(c.trust.public.per_task_usd, Some(0.5));
        assert_eq!(c.trust.public.per_initiative_usd, Some(3.0));
        assert_eq!(c.trust.public.workflows, Some(vec!["direct".to_string()]));
        assert!(c.trust.public.allow_protected);
        assert_eq!(c.trust.public.egress, TrustEgress::Declared);
        assert_eq!(c.trust.public.per_day, Some(10));
        assert!(c.trust.public.auto_land);
        // Untouched levels keep their own defaults.
        assert_eq!(c.trust.operator.egress, TrustEgress::Declared);
        assert!(c.trust.operator.allow_protected);
    }

    #[test]
    fn trust_egress_as_str_matches_the_config_spelling() {
        assert_eq!(TrustEgress::Model.as_str(), "model");
        assert_eq!(TrustEgress::Declared.as_str(), "declared");
    }

    #[test]
    fn an_unknown_trust_egress_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[trust.public]\negress = \"anywhere\"\n",
        )
        .unwrap();
        let err = match load_home(dir.path()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("trust.public.egress"), "{err}");
        assert!(err.contains("anywhere"), "{err}");
    }
}
