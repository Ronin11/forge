//! `forge task set`: changing a queued or blocked task in place, each new
//! value held to what `enqueue` holds it to.

use super::*;

/// What `forge task set` may change on a queued or blocked task. Each
/// `Some` replaces the stored value; `None` leaves it. `after` and
/// `checks` replace the whole list, so `Some(vec![])` clears it
/// (`--no-after`, `--no-checks`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TaskEdit {
    pub budget: Option<f64>,
    /// `--allow-over-trust-cap`: `budget` may pass the task's trust level's
    /// `per_task_usd`; the decision row says so.
    pub allow_over_trust_cap: bool,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub retries: Option<u32>,
    pub text: Option<String>,
    pub workflow: Option<String>,
    pub after: Option<Vec<i64>>,
    pub checks: Option<Vec<String>>,
    /// Route every role of the task to this provider by hand.
    pub provider: Option<String>,
    /// See `store::priority`.
    pub priority: Option<i64>,
}

impl TaskEdit {
    pub fn is_empty(&self) -> bool {
        *self == TaskEdit::default()
    }
}

/// Whether `dep` waits, directly or through other tasks, on `id`: the
/// cycle `--after` must not close.
fn waits_on(f: &Forge, dep: i64, id: i64) -> Result<bool> {
    let mut seen = std::collections::BTreeSet::new();
    let mut todo = vec![dep];
    while let Some(next) = todo.pop() {
        if next == id {
            return Ok(true);
        }
        if !seen.insert(next) {
            continue;
        }
        if let Some(t) = f.store.task(next)? {
            todo.extend(t.after);
        }
    }
    Ok(false)
}

/// The dependencies `--after` sets on task `id`, deduplicated in order:
/// each must fit as at enqueue (`dependency_fits`) and must not be this
/// task or wait on it.
async fn after_fits(f: &Forge, id: i64, after: &[i64]) -> Result<Vec<i64>> {
    let mut deps: Vec<i64> = Vec::new();
    for &dep in after {
        if dep == id {
            bail!("--after {dep}: a task cannot wait on itself");
        }
        dependency_fits(f, dep).await?;
        if waits_on(f, dep, id)? {
            bail!(
                "--after {dep}: that task already waits on task {id}; they would wait on each other"
            );
        }
        if !deps.contains(&dep) {
            deps.push(dep);
        }
    }
    Ok(deps)
}

/// `--provider name`: it must be a configured provider; the change is
/// recorded against what routing the task had before (a name, or "by
/// role" when it named none).
fn apply_provider(
    f: &Forge,
    old: &Task,
    name: &str,
    changes: &mut Vec<String>,
    up: &mut crate::store::TaskUpdate,
) -> Result<()> {
    if !f.providers.contains_key(name) {
        bail!("unknown provider {name:?}; see `forge providers` for what is configured");
    }
    let was = Some(old.provider.as_str()).filter(|p| !p.is_empty());
    changes.push(format!("provider {} → {name}", was.unwrap_or("by role")));
    up.provider = Some(name.to_string());
    Ok(())
}

/// `--priority`: already held to 0..7 by `store::parse_priority` at
/// argument-parsing time, so nothing left to validate here.
fn apply_priority(old: i64, p: i64, changes: &mut Vec<String>, up: &mut crate::store::TaskUpdate) {
    changes.push(format!("priority {old} → {p}"));
    up.priority = Some(p);
}

/// Apply `edit` to task `id` in place: the task must be queued or blocked
/// (a running attempt might still finish; a finished task is done), and
/// every new value is held to what `enqueue` holds it to: a positive
/// budget, non-empty text, a workflow that exists, resolves, fits the
/// repository and is allowed at the task's trust level, dependencies that
/// exist, will land and do not wait on this task, and checks that leave
/// something to verify the work. The change is a decision row on the
/// task naming each field's old and new value (text by length and
/// content hash), retry-linked to the task, so `forge decisions` shows
/// it beside an operator's answer. State is untouched: a blocked task
/// stays blocked. Returns the changes as recorded.
pub async fn edit_task(f: &Forge, id: i64, edit: &TaskEdit) -> Result<Vec<String>> {
    if edit.is_empty() {
        bail!(
            "nothing to set: pass --budget, --max-turns, --timeout-secs, --retries, --text, --text-file, --workflow, --after/--no-after, --check/--no-checks, --provider or --priority"
        );
    }
    if let Some(b) = edit.budget
        && (!b.is_finite() || b <= 0.0)
    {
        bail!("budget must be a positive finite number");
    }
    let Some(old) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    if !matches!(old.state, TaskState::Queued | TaskState::Blocked) {
        bail!(
            "task {id} is {}; only a queued or blocked task's spec is set (a running attempt might still finish, and a finished task is done)",
            old.state.as_str()
        );
    }
    let mut changes = Vec::new();
    let mut up = crate::store::TaskUpdate::default();
    if let Some(b) = edit.budget {
        let over = budget_edit_over_trust_cap(f, &old, b, edit.allow_over_trust_cap)?;
        changes.push(format!(
            "budget {} → ${b:.2}{over}",
            old.budget_usd
                .map_or("unset".to_string(), |o| format!("${o:.2}"))
        ));
        up.budget_usd = Some(b);
    }
    if let Some(n) = edit.max_turns {
        changes.push(format!("max-turns {} → {n}", old.max_turns));
        up.max_turns = Some(n as i64);
    }
    if let Some(n) = edit.timeout_secs {
        changes.push(format!("timeout-secs {} → {n}", old.timeout_secs));
        up.timeout_secs = Some(n as i64);
    }
    if let Some(n) = edit.retries {
        changes.push(format!("retries {} → {n}", old.max_attempts - 1));
        up.max_attempts = Some(n as i64 + 1);
    }
    if let Some(text) = &edit.text {
        if text.trim().is_empty() {
            bail!("--text: the task text is empty");
        }
        let describe = |t: &str| {
            format!(
                "{} chars {}",
                t.chars().count(),
                &crate::job::sha256_hex(t.as_bytes())[..12]
            )
        };
        changes.push(format!("text {} → {}", describe(&old.task), describe(text)));
        let path_tokens = text
            .split_whitespace()
            .filter(|w| crate::render::is_path_like_word(w))
            .count() as i64;
        up.task = Some((text.clone(), text.chars().count() as i64, path_tokens));
    }
    if let Some(name) = &edit.provider {
        apply_provider(f, &old, name, &mut changes, &mut up)?;
    }
    if let Some(p) = edit.priority {
        apply_priority(old.priority, p, &mut changes, &mut up);
    }
    let repo = PathBuf::from(&old.repo);
    let cfg = config::load_working(&repo).await?;
    if let Some(name) = &edit.workflow {
        let (wf, resolved) = workflow_fits(f, &cfg, name)?;
        let policy = match old.trust {
            crate::store::Trust::Operator => &f.trust.operator,
            crate::store::Trust::Contact => &f.trust.contact,
            crate::store::Trust::Public => &f.trust.public,
        };
        workflow_allowed(old.trust, policy, name)?;
        changes.push(format!("workflow {} → {name}", old.workflow));
        up.tdd = Some(resolved.steps.iter().any(|s| s.action.name == "tests"));
        up.workflow = Some((name.clone(), wf.hash.clone(), wf.text.clone()));
    }
    if let Some(after) = &edit.after {
        let deps = after_fits(f, id, after).await?;
        changes.push(format!("after {:?} → {deps:?}", old.after));
        up.after = Some(deps);
    }
    if let Some(checks) = &edit.checks {
        if checks.iter().any(|c| c.trim().is_empty()) {
            bail!("--check: a check command is empty");
        }
        if checks.is_empty() && cfg.checks.is_empty() {
            bail!(
                "{} declares no [checks] and --no-checks leaves the task none; nothing would verify the work",
                cfg.config_path
            );
        }
        changes.push(format!(
            "checks {} → {}",
            serde_json::to_string(&old.checks)?,
            serde_json::to_string(checks)?
        ));
        up.checks = Some(checks.clone());
    }
    if !f.store.set_task_fields(id, &up)? {
        bail!("task {id} changed state before its spec could be set");
    }
    let decision = f.store.insert_decision_by(crate::store::InsertDecisionBy {
        task_id: id,
        repo: &old.repo,
        question: &format!("task {id}'s spec"),
        answer: &format!("set {}", changes.join(", ")),
        answered_by: "operator",
        citations: "",
        answered_for: old.question_to.as_deref(),
    })?;
    f.store.set_decision_retry(decision, id)?;
    Ok(changes)
}
