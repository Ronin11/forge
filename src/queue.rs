//! The queue: how a task comes to exist. Every way in (`forge add`,
//! `forge run`, `forge retry`, `forge answer`, the supervisor) builds a
//! `TaskRequest`, and `enqueue` validates it against the repository and
//! the workflow directory, inserts it, carries a retried task's
//! dependents along, and emits `task_queued`. The CLI's argument struct
//! converts into a request; nothing here knows about clap.

use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{Task, TaskState};
use crate::{config, git, unix_now, workflows};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// What a new task is made from. Field names follow the CLI flags; the
/// negatives (`no_land`, `no_journal`, `no_context`) are the operator's
/// control arms and default to off.
#[derive(Debug, Clone, Default)]
pub struct TaskRequest {
    pub repo: PathBuf,
    pub task: String,
    pub model: String,
    pub max_turns: u32,
    pub retries: u32,
    pub timeout_secs: u32,
    pub budget: Option<f64>,
    pub checks: Vec<String>,
    pub allow_protected: bool,
    pub workflow: String,
    pub show_checks: bool,
    pub no_land: bool,
    pub after: Vec<i64>,
    pub no_journal: bool,
    pub no_context: bool,
    pub resume_on_failure: bool,
}

pub async fn enqueue(f: &Forge, args: &TaskRequest, retry_of: Option<i64>) -> Result<Task> {
    if let Some(b) = args.budget
        && b <= 0.0
    {
        bail!("budget must be positive");
    }
    let repo = args.repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let cfg = config::load_working(&repo).await?;
    let wf = workflows::get(&f.paths.home, &args.workflow)?.with_context(|| {
        format!(
            "unknown workflow {:?}; see `forge workflows`",
            args.workflow
        )
    })?;
    // Resolution happens at start; here it only has to be possible, and the
    // whole directory has to be sound: one broken file blocks every task.
    let problems = workflows::check(&f.paths.home)?;
    if let Some(p) = problems.iter().find(|p| p.blocking) {
        bail!(
            "workflow directory is broken: {} {} (forge doctor lists all)",
            p.file,
            p.what
        );
    }
    let resolved = workflows::resolve(&f.paths.home, &args.workflow)?;
    if resolved.steps.iter().any(|s| s.action.name == "tests") {
        if cfg.namespace.is_empty() {
            bail!(
                "the {} workflow needs [verify] namespace in forge.toml: where the tests step may write",
                wf.name
            );
        }
        if !cfg.checks.contains_key("test") {
            bail!(
                "the {} workflow needs a check named `test` in forge.toml: what runs the hidden tests",
                wf.name
            );
        }
    }
    if !cfg.namespace.is_empty() {
        let present = git::ls_tree(&repo, &cfg.base_branch, &cfg.namespace).await?;
        if !present.is_empty() {
            bail!(
                "the verification namespace ({}) must not exist on {}; it is overlaid at verify time. Found: {}",
                cfg.namespace.join(", "),
                cfg.base_branch,
                present.join(", ")
            );
        }
    }
    if cfg.checks.is_empty() && args.checks.is_empty() {
        bail!(
            "{} declares no [checks] and the task declares no --check; nothing would verify the work",
            cfg.config_path
        );
    }
    let mut t = Task {
        repo: repo.display().to_string(),
        task: args.task.clone(),
        base_branch: cfg.base_branch.clone(),
        model: args.model.clone(),
        max_turns: args.max_turns as i64,
        max_attempts: args.retries as i64 + 1,
        timeout_secs: args.timeout_secs as i64,
        checks: args.checks.clone(),
        state: TaskState::Queued,
        created_at: unix_now(),
        budget_usd: args.budget,
        allow_protected: args.allow_protected,
        workflow: args.workflow.clone(),
        workflow_hash: wf.hash.clone(),
        workflow_text: wf.text.clone(),
        show_checks: args.show_checks,
        land: !args.no_land,
        after: args.after.clone(),
        journal: !args.no_journal,
        context_enabled: !args.no_context,
        resume_on_failure: args.resume_on_failure,
        retry_of,
        ..Default::default()
    };
    for &dep in &t.after {
        let Some(d) = f.store.task(dep)? else {
            bail!("--after {dep}: no such task");
        };
        if d.repo != t.repo {
            bail!(
                "--after {dep}: that task is in {}, not this repository",
                d.repo
            );
        }
        if !d.land && d.state != TaskState::Succeeded {
            bail!(
                "--after {dep}: that task will not land (--no-land), so nothing built on it could see its work"
            );
        }
    }
    t.id = f.store.insert_task(&t)?;
    f.report.emit(
        t.id,
        Event::TaskQueued {
            workflow: &t.workflow,
            retry_of,
        },
    );
    if let Some(old) = retry_of {
        // Whatever waited on the task this one retries now waits on this
        // one; a dependent swept into blocked when the old task ended is
        // queued again.
        for d in f.store.reroute_dependents(old, t.id)? {
            f.report.emit(
                d,
                Event::Note {
                    text: &format!("waits on task {} now (a retry of task {old})", t.id),
                },
            );
        }
    }
    Ok(t)
}

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
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub workflow: Option<String>,
}

impl RetryOverrides {
    pub fn none() -> RetryOverrides {
        RetryOverrides {
            retries: None,
            budget: None,
            max_turns: None,
            timeout_secs: None,
            workflow: None,
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
        model: t.model.clone(),
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
        checks: t.checks.clone(),
        allow_protected: t.allow_protected,
        workflow: if first {
            o.workflow.clone().unwrap_or(t.workflow.clone())
        } else {
            t.workflow.clone()
        },
        show_checks: t.show_checks,
        no_land: !t.land,
        no_journal: !t.journal,
        no_context: !t.context_enabled,
        resume_on_failure: t.resume_on_failure,
        after,
    }
}

/// Answer a task blocked on a question: record the decision, re-queue
/// the task as a retry whose text carries the answer, and point the
/// decision at it. `by` is "operator" or "supervisor"; `citations` is
/// what a supervisor's answer rests on. Returns the decision and the
/// new task.
pub async fn answer(
    f: &Forge,
    id: i64,
    text: &str,
    by: &str,
    citations: &str,
) -> Result<(i64, Task)> {
    let Some(old) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    let last = f
        .store
        .attempts(id)?
        .into_iter()
        .rev()
        .find(|a| a.step != "supervisor");
    if old.state != TaskState::Blocked
        || !matches!(
            last.as_ref().map(|a| a.state),
            Some(crate::store::AttemptState::NeedsInput)
        )
    {
        bail!(
            "task {id} is not blocked on a question (state {}); only that is answered",
            old.state.as_str()
        );
    }
    let question = last
        .and_then(|a| serde_json::from_str::<crate::envelope::Envelope>(&a.envelope_json).ok())
        .and_then(|e| e.needs_input)
        .map(|q| q.question)
        .with_context(|| format!("task {id}'s last attempt recorded no question"))?;
    let decision = f
        .store
        .insert_decision_by(id, &old.repo, &question, text, by, citations)?;
    let new_text = if by == "operator" {
        format!(
            "{}\n\nOperator's answer to a question from an earlier attempt: {text}",
            old.task
        )
    } else {
        format!(
            "{}\n\nSupervisor's answer to a question from an earlier attempt (citing {citations}): {text}",
            old.task
        )
    };
    let after = old
        .after
        .iter()
        .map(|&d| map_dep(f, d, &std::collections::HashMap::new()))
        .collect::<Result<Vec<_>>>()?;
    let req = retry_request(&old, &RetryOverrides::none(), true, after, Some(new_text));
    let n = enqueue(f, &req, Some(id)).await?;
    f.store.set_decision_retry(decision, n.id)?;
    Ok((decision, n))
}
