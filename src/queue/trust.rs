//! What a task's trust level allows: the `[trust.<level>]` policy held
//! against a request at enqueue and against a budget edit, and the
//! decision row recorded when the operator lets a budget past its cap.

use super::*;

/// The level's own `[trust.<level>]` policy, applied at enqueue: a
/// workflow outside `policy.workflows` is refused naming the level and
/// the list; `allow_protected` is refused where the level forbids it.
/// `per_day` is enforced separately, inside `Store::insert_task_armed`'s
/// transaction, so two concurrent filers cannot both pass a check made
/// here and then both insert. Returns the budget to actually record:
/// The one clause of `apply_trust_policy` an edit re-checks: a workflow
/// outside `policy.workflows` is refused naming the level and the list.
pub(super) fn workflow_allowed(
    level: crate::store::Trust,
    policy: &config::TrustPolicy,
    workflow: &str,
) -> Result<()> {
    if let Some(allowed) = &policy.workflows
        && !allowed.iter().any(|w| w == workflow)
    {
        bail!(
            "trust {}: workflow {workflow:?} is not allowed at this level; allowed: {}",
            level.as_str(),
            allowed.join(", ")
        );
    }
    Ok(())
}

/// What a task at `level` may spend, given the `--budget` it asked for:
/// the level's `per_task_usd` when it named none, the smaller of the two
/// otherwise, or the asked amount over the cap only when
/// `allow_over_trust_cap` (the operator's flag) says so. A level that
/// names no cap leaves `budget` unchanged. The workflow and protected-path
/// clauses are refused here too. Pure and argument-driven so every field
/// is unit tested without a store or a config file.
fn apply_trust_policy(
    level: crate::store::Trust,
    policy: &config::TrustPolicy,
    workflow: &str,
    allow_protected: bool,
    budget: Option<f64>,
    allow_over_trust_cap: bool,
) -> Result<Option<f64>> {
    workflow_allowed(level, policy, workflow)?;
    if allow_protected && !policy.allow_protected {
        bail!(
            "trust {}: --allow-protected is not allowed at this level",
            level.as_str()
        );
    }
    let Some(cap) = policy.per_task_usd else {
        return Ok(budget);
    };
    if let Some(b) = budget
        && b > cap
        && !allow_over_trust_cap
    {
        bail!(
            "trust {}: --budget ${b:.2} is over this level's per_task_usd cap of ${cap:.2}; \
             --allow-over-trust-cap (operator only) raises it",
            level.as_str()
        );
    }
    Ok(Some(budget.unwrap_or(cap)))
}

/// The amount by which `budget` was let past the level's `per_task_usd`
/// cap by `--allow-over-trust-cap`, as `(budget, cap)`; `None` when the
/// task is within its level's cap.
fn over_trust_cap(policy: &config::TrustPolicy, budget: Option<f64>) -> Option<(f64, f64)> {
    match (budget, policy.per_task_usd) {
        (Some(b), Some(cap)) if b > cap => Some((b, cap)),
        _ => None,
    }
}

/// An initiative filing more tasks at a level is refused once its tasks
/// have cost the level's `per_initiative_usd` between them.
fn initiative_within_trust_cap(
    f: &Forge,
    level: crate::store::Trust,
    policy: &config::TrustPolicy,
    initiative: Option<i64>,
) -> Result<()> {
    let (Some(cap), Some(id)) = (policy.per_initiative_usd, initiative) else {
        return Ok(());
    };
    let spent = f.store.initiative_cost(id)?;
    if spent >= cap {
        bail!(
            "trust {}: initiative {id} has spent ${spent:.2} of this level's per_initiative_usd cap of ${cap:.2}",
            level.as_str()
        );
    }
    Ok(())
}

/// What the request's trust level decides at enqueue: the budget the task
/// files with, the level's `per_day` cap, and, when the operator's flag let
/// the budget past the level's cap, the `(budget, cap)` to record.
pub(super) struct TrustGate {
    pub(super) budget: Option<f64>,
    pub(super) per_day: Option<u32>,
    pub(super) over_cap: Option<(f64, f64)>,
}

pub(super) fn trust_gate(
    f: &Forge,
    args: &TaskRequest,
    level: crate::store::Trust,
    workflow: &str,
    initiative: Option<i64>,
) -> Result<TrustGate> {
    let policy = match level {
        crate::store::Trust::Operator => &f.trust.operator,
        crate::store::Trust::Contact => &f.trust.contact,
        crate::store::Trust::Public => &f.trust.public,
    };
    let budget = apply_trust_policy(
        level,
        policy,
        workflow,
        args.allow_protected,
        args.budget,
        args.allow_over_trust_cap,
    )?;
    initiative_within_trust_cap(f, level, policy, initiative)?;
    Ok(TrustGate {
        budget,
        per_day: policy.per_day,
        over_cap: over_trust_cap(policy, budget),
    })
}

/// Record that the operator let task `t` past its level's cap, as a
/// decision row on the task (see `forge decisions`); nothing when `over`
/// is `None`.
pub(super) fn record_over_trust_cap(f: &Forge, t: &Task, over: Option<(f64, f64)>) -> Result<()> {
    let Some(over) = over else { return Ok(()) };
    let decision = f.store.insert_decision_by(crate::store::InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("task {}'s budget over its trust cap", t.id),
        answer: &format!(
            "allowed ${:.2}, over the {} level's per_task_usd cap of ${:.2} (--allow-over-trust-cap)",
            over.0,
            t.trust.as_str(),
            over.1
        ),
        answered_by: "operator",
        citations: "",
        answered_for: None,
    })?;
    f.store.set_decision_retry(decision, t.id)
}

/// A budget edit holds to the task's trust level like a new task does:
/// over the level's `per_task_usd` it is refused unless the operator
/// passed `--allow-over-trust-cap`, and then the decision row says so
/// (the returned suffix, empty within the cap).
pub(super) fn budget_edit_over_trust_cap(
    f: &Forge,
    t: &Task,
    b: f64,
    allow: bool,
) -> Result<String> {
    let policy = match t.trust {
        crate::store::Trust::Operator => &f.trust.operator,
        crate::store::Trust::Contact => &f.trust.contact,
        crate::store::Trust::Public => &f.trust.public,
    };
    let Some((_, cap)) = over_trust_cap(policy, Some(b)) else {
        return Ok(String::new());
    };
    if !allow {
        bail!(
            "trust {}: --budget ${b:.2} is over this level's per_task_usd cap of ${cap:.2}; \
             --allow-over-trust-cap (operator only) raises it",
            t.trust.as_str()
        );
    }
    Ok(format!(
        " (over the {} level's ${cap:.2} cap, --allow-over-trust-cap)",
        t.trust.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unrestricted_policy() -> config::TrustPolicy {
        config::TrustPolicy {
            per_task_usd: None,
            per_initiative_usd: None,
            workflows: None,
            allow_protected: true,
            egress: config::TrustEgress::Declared,
            per_day: None,
            auto_land: true,
            allow_unsandboxed: false,
        }
    }

    #[test]
    fn apply_trust_policy_allows_any_workflow_when_the_level_names_none() {
        let policy = unrestricted_policy();
        assert!(
            apply_trust_policy(
                crate::store::Trust::Public,
                &policy,
                "direct",
                false,
                None,
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn apply_trust_policy_refuses_a_workflow_outside_the_levels_list_naming_the_level_and_the_list()
    {
        let policy = config::TrustPolicy {
            workflows: Some(vec!["reviewed".to_string()]),
            ..unrestricted_policy()
        };
        let err = apply_trust_policy(
            crate::store::Trust::Public,
            &policy,
            "direct",
            false,
            None,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("public"), "{err}");
        assert!(err.contains("direct"), "{err}");
        assert!(err.contains("reviewed"), "{err}");
    }

    #[test]
    fn apply_trust_policy_allows_a_workflow_named_in_the_levels_list() {
        let policy = config::TrustPolicy {
            workflows: Some(vec!["reviewed".to_string(), "tdd-reviewed".to_string()]),
            ..unrestricted_policy()
        };
        assert!(
            apply_trust_policy(
                crate::store::Trust::Contact,
                &policy,
                "tdd-reviewed",
                false,
                None,
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn apply_trust_policy_refuses_allow_protected_where_the_level_forbids_it() {
        let policy = config::TrustPolicy {
            allow_protected: false,
            ..unrestricted_policy()
        };
        let err = apply_trust_policy(
            crate::store::Trust::Public,
            &policy,
            "direct",
            true,
            None,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("public"), "{err}");
        assert!(err.contains("allow-protected"), "{err}");
    }

    #[test]
    fn apply_trust_policy_allows_allow_protected_where_the_level_permits_it() {
        let policy = unrestricted_policy();
        assert!(
            apply_trust_policy(
                crate::store::Trust::Operator,
                &policy,
                "direct",
                true,
                None,
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn apply_trust_policy_does_not_mind_allow_protected_unset_regardless_of_the_level() {
        let policy = config::TrustPolicy {
            allow_protected: false,
            ..unrestricted_policy()
        };
        assert!(
            apply_trust_policy(
                crate::store::Trust::Public,
                &policy,
                "direct",
                false,
                None,
                false
            )
            .is_ok()
        );
    }

    #[test]
    fn apply_trust_policy_leaves_the_budget_alone_when_the_level_names_no_cap() {
        let policy = unrestricted_policy();
        let got = apply_trust_policy(
            crate::store::Trust::Operator,
            &policy,
            "direct",
            false,
            None,
            false,
        )
        .unwrap();
        assert_eq!(got, None);
        let got = apply_trust_policy(
            crate::store::Trust::Operator,
            &policy,
            "direct",
            false,
            Some(50.0),
            false,
        )
        .unwrap();
        assert_eq!(got, Some(50.0));
    }

    #[test]
    fn apply_trust_policy_uses_the_levels_cap_when_the_request_names_no_budget() {
        let policy = config::TrustPolicy {
            per_task_usd: Some(1.0),
            ..unrestricted_policy()
        };
        let got = apply_trust_policy(
            crate::store::Trust::Public,
            &policy,
            "direct",
            false,
            None,
            false,
        )
        .unwrap();
        assert_eq!(got, Some(1.0));
    }

    #[test]
    fn apply_trust_policy_refuses_a_requested_budget_above_the_levels_cap() {
        let policy = config::TrustPolicy {
            per_task_usd: Some(1.0),
            ..unrestricted_policy()
        };
        let err = apply_trust_policy(
            crate::store::Trust::Public,
            &policy,
            "direct",
            false,
            Some(10.0),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--allow-over-trust-cap"), "{err}");
        assert!(err.contains("$1.00"), "{err}");
    }

    #[test]
    fn apply_trust_policy_lets_the_operator_flag_past_the_levels_cap() {
        let policy = config::TrustPolicy {
            per_task_usd: Some(1.0),
            ..unrestricted_policy()
        };
        let got = apply_trust_policy(
            crate::store::Trust::Public,
            &policy,
            "direct",
            false,
            Some(10.0),
            true,
        )
        .unwrap();
        assert_eq!(got, Some(10.0));
        assert_eq!(over_trust_cap(&policy, got), Some((10.0, 1.0)));
        assert_eq!(over_trust_cap(&policy, Some(1.0)), None);
    }

    #[test]
    fn apply_trust_policy_leaves_a_requested_budget_under_the_levels_cap() {
        let policy = config::TrustPolicy {
            per_task_usd: Some(5.0),
            ..unrestricted_policy()
        };
        let got = apply_trust_policy(
            crate::store::Trust::Public,
            &policy,
            "direct",
            false,
            Some(2.0),
            false,
        )
        .unwrap();
        assert_eq!(got, Some(2.0));
    }
}
