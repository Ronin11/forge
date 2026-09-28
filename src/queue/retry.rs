//! What a re-queued task is made from: the request a retry files, the
//! overrides it may carry, and where each of its dependencies now points.

use super::*;

/// A dependency for a re-queued task: the same one if it landed, the
/// newest retry of it if there is one, else a refusal naming it.
pub fn map_dep(f: &Forge, d: i64, made: &std::collections::HashMap<i64, i64>) -> Result<i64> {
    if let Some(&n) = made.get(&d) {
        return Ok(n);
    }
    let Some(dep) = f.store.task(d)? else {
        bail!("dependency {d} does not exist");
    };
    if dep.state == TaskState::Succeeded && (!dep.land || !dep.landed_sha.is_empty()) {
        return Ok(d);
    }
    if matches!(dep.state, TaskState::Queued | TaskState::Running) {
        return Ok(d);
    }
    if let Some(n) = f.store.latest_retry_of(d)? {
        return Ok(n);
    }
    bail!(
        "dependency {d} ended without landing ({}); retry it first and this task will follow it",
        dep.state.as_str()
    )
}

/// What a retry may change about the first task it re-queues; chained
/// dependents keep their own settings.
pub struct RetryOverrides {
    pub retries: Option<u32>,
    pub budget: Option<f64>,
    /// The operator's flag letting `budget` past the trust level's cap.
    pub allow_over_trust_cap: bool,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub workflow: Option<String>,
    pub provider: Option<String>,
}

impl RetryOverrides {
    pub fn none() -> RetryOverrides {
        RetryOverrides {
            retries: None,
            budget: None,
            allow_over_trust_cap: false,
            max_turns: None,
            timeout_secs: None,
            workflow: None,
            provider: None,
        }
    }
}

/// The `TaskArgs` a retry of `t` re-queues with: `first` is whether `t` is
/// the task the operator named (only that one takes the overrides and a
/// text override; chained dependents keep their own settings and text).
pub fn retry_request(
    t: &Task,
    o: &RetryOverrides,
    first: bool,
    after: Vec<i64>,
    task: Option<String>,
) -> TaskRequest {
    TaskRequest {
        repo: PathBuf::from(&t.repo),
        task: task.unwrap_or_else(|| t.task.clone()),
        title: t.title.clone(),
        model: Some(t.model.clone()),
        // Empty means the original task named no `--provider` and
        // resolved per role; a retry should resolve the same way, not
        // pin whatever "code" happened to pick at the time.
        provider: o
            .provider
            .clone()
            .filter(|_| first)
            .or_else(|| (!t.provider.is_empty()).then(|| t.provider.clone())),
        max_turns: if first {
            o.max_turns.unwrap_or(t.max_turns as u32)
        } else {
            t.max_turns as u32
        },
        retries: if first {
            o.retries.unwrap_or((t.max_attempts - 1).max(0) as u32)
        } else {
            (t.max_attempts - 1).max(0) as u32
        },
        timeout_secs: if first {
            o.timeout_secs.unwrap_or(t.timeout_secs as u32)
        } else {
            t.timeout_secs as u32
        },
        budget: if first {
            o.budget.or(t.budget_usd)
        } else {
            t.budget_usd
        },
        // A budget already decided is re-filed as it was; one named on
        // `forge retry --budget` is a new request and needs the flag.
        allow_over_trust_cap: !(first && o.budget.is_some()) || o.allow_over_trust_cap,
        checks: t.checks.clone(),
        allow_protected: t.allow_protected,
        workflow: Some(if first {
            o.workflow.clone().unwrap_or(t.workflow.clone())
        } else {
            t.workflow.clone()
        }),
        project: t.project.clone(),
        initiative: t.initiative,
        show_checks: t.show_checks,
        no_land: !t.land,
        // A retry keeps the arm it started with rather than drawing again.
        journal_choice: Some(t.journal),
        no_context: !t.context_enabled,
        resume_on_failure: t.resume_on_failure,
        // A retry keeps the trust of the task it retries: the caller
        // running `forge retry` did not file the original request.
        trust: Some(t.trust.as_str().to_string()),
        after,
        blocked: None,
        // A retry or a refile keeps the priority of the task it
        // re-queues; it is never drawn or defaulted again.
        priority: Some(t.priority),
        // A retry re-queues `t` itself, not a task replacing it; the
        // supersedes link is only for `forge add --supersedes`.
        supersedes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(priority: i64) -> Task {
        Task {
            repo: "/r".into(),
            task: "do it".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            priority,
            ..Default::default()
        }
    }

    #[test]
    fn retry_request_inherits_the_predecessors_priority() {
        let t = task(7);
        let req = retry_request(&t, &RetryOverrides::none(), true, vec![], None);
        assert_eq!(req.priority, Some(7));

        let t = task(0);
        let req = retry_request(&t, &RetryOverrides::none(), false, vec![], None);
        assert_eq!(req.priority, Some(0));
    }
}
