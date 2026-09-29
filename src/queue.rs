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
use std::collections::BTreeMap;
use std::path::PathBuf;

mod answer;
mod arms;
mod duplicates;
mod edit;
mod initiative;
mod job_question;
mod retry;
mod trust;
pub use answer::answer;
use arms::{assign_explore, assign_journal_arm};
pub use duplicates::{refuse_live_descendant, withdraw_abort};
pub use edit::{TaskEdit, edit_task};
pub use initiative::{
    FileTask, file_initiative_paragraphs, file_plan, parse_initiative_file,
    validate_initiative_file,
};
pub use retry::{RetryOverrides, map_dep, retry_request};
use trust::{
    TrustGate, budget_edit_over_trust_cap, record_over_trust_cap, trust_gate, workflow_allowed,
};

/// What a new task is made from. Field names follow the CLI flags; the
/// negatives (`no_land`, `no_context`) are the operator's control arms
/// and default to off.
#[derive(Debug, Clone, Default)]
pub struct TaskRequest {
    pub repo: PathBuf,
    pub task: String,
    /// The task in the customer's own words, for the day it was filed
    /// that way (`--title`, or the concierge on a filed request); `None`
    /// leaves it unset (see docs/PORTAL.md).
    pub title: Option<String>,
    /// `None` takes the model from the selected provider's own default
    /// (`--model` always wins when given).
    pub model: Option<String>,
    /// The provider every agent step of the task runs under (the
    /// supervisor keeps its own); `None` is the built-in "anthropic".
    pub provider: Option<String>,
    pub max_turns: u32,
    pub retries: u32,
    pub timeout_secs: u32,
    pub budget: Option<f64>,
    /// The operator's `--allow-over-trust-cap`: `budget` may exceed the
    /// trust level's `per_task_usd`, recorded as a decision. Only the CLI
    /// sets it; every other way in files at the level's cap or less.
    pub allow_over_trust_cap: bool,
    pub checks: Vec<String>,
    pub allow_protected: bool,
    /// The workflow to run; `None` falls to the project's default, then
    /// "direct" (see docs/PROJECTS.md, "Configuration layering").
    pub workflow: Option<String>,
    /// The project this task belongs to; `None` falls to the
    /// repository's default project (`Store::ensure_default_project`).
    pub project: Option<String>,
    /// The initiative this task belongs to, if any; when set, its
    /// project wins over `project` (which must then either agree or be
    /// unset — see docs/PROJECTS.md, "Verbs").
    pub initiative: Option<i64>,
    pub show_checks: bool,
    pub no_land: bool,
    pub after: Vec<i64>,
    /// 0 (lowest) to 7 (highest); `None` takes `store::PRIORITY_DEFAULT`
    /// (see `store::priority`). A retry or a refile (`retry_request`)
    /// names the task it re-queues' own value here instead.
    pub priority: Option<i64>,
    /// Whether the request said `--journal` (`Some(true)`) or
    /// `--no-journal` (`Some(false)`) itself; `None` when it said
    /// neither, leaving the arm to the operator's control fraction (see
    /// `assign_journal_arm`).
    pub journal_choice: Option<bool>,
    pub no_context: bool,
    pub resume_on_failure: bool,
    /// The trust the caller earned by the path it queued through:
    /// `"operator"`, `"contact"`, or `"public"` (see `store::Trust`).
    /// `None` is the CLI's own default, operator. A retry propagates the
    /// task it retries' own trust rather than resolving this again.
    pub trust: Option<String>,
    /// The concierge or escalator already knows this task starts
    /// blocked, with a question (`concierge::ask`'s "unclear" branch and
    /// `file_proposal`): `enqueue` inserts it straight into `Blocked`
    /// with this reason and contact, never briefly `queued` and
    /// claimable, so a worker can never start a run meant to wait for an
    /// answer (see docs/REVIEW-3.md, defect 1). `None` inserts `Queued`,
    /// as every other request does.
    pub blocked: Option<BlockedInit>,
}

/// See `TaskRequest::blocked`.
#[derive(Debug, Clone)]
pub struct BlockedInit {
    pub reason: String,
    pub question_to: Option<String>,
}

/// Task shape at intake (see docs/ECONOMIST.md, "Task shape"): what the
/// economist must condition on before the task even runs, computed once
/// at `enqueue` and recorded on the task's own `shape_*` columns.
pub struct TaskShape {
    /// The text's length, in characters.
    pub text_len: i64,
    /// How many of the text's whitespace-separated words look like a path
    /// (see `render::is_path_like_word`) — a request naming two paths
    /// counts two.
    pub path_tokens: i64,
    /// Whether the resolved workflow writes hidden tests: a step whose
    /// action is `"tests"`, directly or through composition.
    pub tdd: bool,
    /// The repository's own `[checks]` count, from `forge.toml` (or
    /// `.forge/forge.toml`) at this moment.
    pub declared_checks: i64,
}

fn task_shape(text: &str, resolved: &workflows::Resolved, declared_checks: usize) -> TaskShape {
    TaskShape {
        text_len: text.chars().count() as i64,
        path_tokens: text
            .split_whitespace()
            .filter(|w| crate::render::is_path_like_word(w))
            .count() as i64,
        tdd: resolved.steps.iter().any(|s| s.action.name == "tests"),
        declared_checks: declared_checks as i64,
    }
}

/// The workflow `name` exists, the directory it lives in is sound, it
/// resolves, and the repository can carry it (a `tests` step needs a
/// namespace and a `test` check). Resolution proper happens at start;
/// here it only has to be possible. Shared by `enqueue` and `edit_task`.
fn workflow_fits(
    f: &Forge,
    cfg: &config::Config,
    name: &str,
) -> Result<(workflows::Workflow, workflows::Resolved)> {
    let wf = workflows::get(&f.paths.home, name)?
        .with_context(|| crate::adopt::unknown_workflow(name))?;
    // One broken file blocks every task.
    let problems = workflows::check(&f.paths.home)?;
    if let Some(p) = problems.iter().find(|p| p.blocking) {
        bail!(
            "workflow directory is broken: {} {} (forge doctor lists all)",
            p.file,
            p.what
        );
    }
    let resolved = workflows::resolve(&f.paths.home, name)?;
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
    Ok((wf, resolved))
}

/// What `--after DEP` must hold, at enqueue and at edit: the task exists
/// and will land. A dependency means only "wait for that task to reach a
/// terminal state; block if it failed" (see `Store::queued_unblocked` and
/// `Store::block_dependents`, both keyed on the dependency's id and state
/// alone), so it carries across repositories: a task on one repository
/// may wait on a task in another.
async fn dependency_fits(f: &Forge, dep: i64) -> Result<()> {
    let Some(d) = f.store.task(dep)? else {
        bail!("--after {dep}: no such task");
    };
    if !d.land && d.state != TaskState::Succeeded {
        bail!(
            "--after {dep}: that task will not land (--no-land), so nothing built on it could see its work"
        );
    }
    // A repository with no push remote never lands anything either
    // (engine::land skips it, leaving `landed_sha` empty), so a
    // dependent would wait on landing that never comes.
    let cfg = config::load_working(std::path::Path::new(&d.repo)).await?;
    if cfg.push_remote.is_none() {
        bail!(
            "--after {dep}: {} has no push remote, so nothing built on it could see its work",
            d.repo
        );
    }
    Ok(())
}

/// `args.priority`, defaulted and range-checked: `store::parse_priority`
/// already does this for the CLI's own `--priority`, but a priority
/// reaching `enqueue` any other way (a retry, a refile, an initiative's
/// default) gets the same guarantee here.
fn resolve_priority(p: Option<i64>) -> Result<i64> {
    let p = p.unwrap_or(crate::store::PRIORITY_DEFAULT);
    if !(crate::store::PRIORITY_MIN..=crate::store::PRIORITY_MAX).contains(&p) {
        bail!(
            "priority must be between {} and {}",
            crate::store::PRIORITY_MIN,
            crate::store::PRIORITY_MAX
        );
    }
    Ok(p)
}

pub async fn enqueue(f: &Forge, args: &TaskRequest, retry_of: Option<i64>) -> Result<Task> {
    if let Some(b) = args.budget
        && b <= 0.0
    {
        bail!("budget must be positive");
    }
    let priority = resolve_priority(args.priority)?;
    let repo = args.repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let repo_str = repo.display().to_string();
    let initiative = args
        .initiative
        .map(|id| {
            f.store
                .initiative(id)?
                .with_context(|| format!("no initiative {id}"))
        })
        .transpose()?;
    // A task named a project directly, or falls to its repository's
    // default project, created the first time the repository is seen; a
    // repository listed by several projects is ambiguous and must be
    // told which with --project (see docs/PROJECTS.md, "Migration"). A
    // task named an initiative instead follows that initiative's project
    // (see docs/PROJECTS.md, "Verbs"): --project must then agree or be
    // absent.
    let project_name = if let Some(ini) = &initiative {
        if let Some(p) = &args.project
            && p != &ini.project
        {
            bail!(
                "--project {p} does not match initiative {}'s project ({})",
                ini.id,
                ini.project
            );
        }
        ini.project.clone()
    } else {
        match &args.project {
            Some(p) => {
                f.store
                    .project(p)?
                    .with_context(|| format!("no project {p}"))?;
                p.clone()
            }
            None => match f.store.ensure_default_project(&repo_str)? {
                Some(name) => name,
                None => {
                    let names = f.store.projects_listing_repo(&repo_str)?;
                    bail!(
                        "{repo_str} is listed by several projects ({}); pass --project to say which",
                        names.join(", ")
                    );
                }
            },
        }
    };
    let project = f.store.project(&project_name)?;
    // Configuration layering: operator config, repository config, the
    // project's defaults, then the task's own flags win (see
    // docs/PROJECTS.md, "Configuration layering"). The workflow has no
    // operator- or repository-level default, so only the last two layers
    // apply here.
    // How `workflow` got its value, for `Task::workflow_source` (see
    // docs/ECONOMIST.md, "The routing record"): named directly on this
    // request, else the project's own default, else the built-in
    // fallback — never the operator's, since no such layer exists here.
    let workflow_source = if args.workflow.is_some() {
        "flag"
    } else if project.as_ref().is_some_and(|p| p.workflow.is_some()) {
        "project"
    } else {
        "default"
    };
    let workflow = args
        .workflow
        .clone()
        .or_else(|| project.as_ref().and_then(|p| p.workflow.clone()))
        .unwrap_or_else(|| "direct".to_string());
    let cfg = config::load_working(&repo).await?;
    let (wf, resolved) = workflow_fits(f, &cfg, &workflow)?;
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
    // `provider` is the task's own flag: every role runs under it when
    // set (validated here so a typo fails at creation, not mid-task).
    // Unset (""), each step's role resolves through its project and the
    // operator's [roles] table instead (see `ctx::resolve_provider`).
    // The default model, when `--model` says nothing either, comes from
    // "code"'s resolved provider: the role that would run first for
    // almost every workflow, and the one every existing config already
    // names when nothing overrides it.
    let provider_name = args.provider.clone().unwrap_or_default();
    if let Some(p) = &args.provider {
        f.providers.get(p).with_context(|| {
            format!("unknown provider {p:?}; see `forge providers` for what is configured")
        })?;
    }
    let project_roles = project
        .as_ref()
        .map(|p| p.role_providers.clone())
        .unwrap_or_default();
    let (code_provider, code_provider_source) = crate::ctx::resolve_provider_routed(
        &f.providers,
        &f.roles,
        &project_roles,
        &BTreeMap::new(),
        &provider_name,
        "code",
    )?;
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| code_provider.model.clone().unwrap_or_default());
    // Where `model` came from, for `Task::model_source`: `--model` itself,
    // else the same layer that resolved "code"'s provider, since that
    // provider's own configured model is what `model` fell back to.
    let model_source = if args.model.is_some() {
        "flag"
    } else {
        code_provider_source
    };
    let shape = task_shape(&args.task, &resolved, cfg.checks.len());
    let trust = match &args.trust {
        Some(s) => crate::store::Trust::try_from(s.as_str())
            .with_context(|| format!("--trust {s:?}: must be operator, contact, or public"))?,
        None => crate::store::Trust::Operator,
    };
    let initiative_id = initiative.as_ref().map(|i| i.id);
    let TrustGate {
        budget,
        per_day: per_day_cap,
        over_cap,
    } = trust_gate(f, args, trust, &workflow, initiative_id)?;
    f.egress_gate(&cfg, trust).map_err(anyhow::Error::msg)?;
    let mut t = Task {
        repo: repo.display().to_string(),
        task: args.task.clone(),
        title: args.title.clone(),
        base_branch: cfg.base_branch.clone(),
        model,
        provider: provider_name,
        max_turns: args.max_turns as i64,
        max_attempts: args.retries as i64 + 1,
        timeout_secs: args.timeout_secs as i64,
        checks: args.checks.clone(),
        state: match &args.blocked {
            Some(_) => TaskState::Blocked,
            None => TaskState::Queued,
        },
        reason: args
            .blocked
            .as_ref()
            .map_or(String::new(), |b| b.reason.clone()),
        question_to: args.blocked.as_ref().and_then(|b| b.question_to.clone()),
        created_at: unix_now(),
        budget_usd: budget,
        allow_protected: args.allow_protected,
        workflow,
        workflow_hash: wf.hash.clone(),
        workflow_text: wf.text.clone(),
        show_checks: args.show_checks,
        land: !args.no_land,
        after: args.after.clone(),
        // Placeholder: the real arm needs the task id, assigned below
        // once it exists.
        journal: true,
        context_enabled: !args.no_context,
        resume_on_failure: args.resume_on_failure,
        retry_of,
        project: Some(project_name),
        initiative: initiative.as_ref().map(|i| i.id),
        shape_text_len: shape.text_len,
        shape_path_tokens: shape.path_tokens,
        shape_tdd: shape.tdd,
        shape_declared_checks: shape.declared_checks,
        model_source: model_source.to_string(),
        workflow_source: workflow_source.to_string(),
        trust,
        priority,
        ..Default::default()
    };
    for &dep in &t.after {
        dependency_fits(f, dep).await?;
    }
    // The operator's `experiment.toml` (piece 4, docs/ECONOMIST.md, "What
    // is built") draws a provider per role for a task that pins no
    // provider of its own; a `--provider` shows deliberate intent, not the
    // default routing the experiment measures. The workflow's source does
    // not matter: the factors are roles, and a task that names its
    // workflow (every initiative-filed task does) still gets its providers
    // drawn. Loaded before the insert, since it only needs the config, not
    // the task's id. Until 2026-09-22 the draw also required the default
    // workflow, and so never fired on a real task.
    let exp = match &args.provider {
        Some(_) => None,
        None => crate::experiment::load(&workflows::catalog_dir(&f.paths.home)?)?,
    };
    let journal_choice = args.journal_choice;
    let journal_fraction = f.measure.journal_control;
    let explicit_provider = args.provider.is_some();
    let explore_cfg = &f.measure.explore;
    let cap = per_day_cap.map(|cap| (t.trust, i64::from(cap), unix_now() - 24 * 3600));
    // Insert and draw the journal and explore arms in one transaction
    // (see docs/REVIEW-3.md, defect 1): the row is claimable, or for a
    // task inserted `Blocked` visible at all, only once its arms are on
    // it, so a claim racing this insert never sees one with an empty
    // `journal_arm`.
    let (id, journal, arm, explore) = f.store.insert_task_armed(&t, cap, |id| {
        let (journal, arm) = assign_journal_arm(id, journal_choice, journal_fraction);
        let mut explore = assign_explore(id, explicit_provider, explore_cfg);
        crate::experiment::extend_explore(&mut explore, id, &project_roles, exp.as_ref());
        Ok((journal, arm.to_string(), explore))
    })?;
    t.id = id;
    t.journal = journal;
    t.journal_arm = arm;
    t.explore = explore;
    record_over_trust_cap(f, &t, over_cap)?;
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

/// Withdraw a task the operator has decided not to do: written against a
/// stale description, superseded, or the product decision went the other
/// way. Only a blocked or queued task is withdrawn — a running attempt
/// might still finish, and a landed task is already merged. Sets a
/// terminal `withdrawn` state with `reason` as the task's own reason,
/// records the reason as a decision row (pointed at the task itself, so
/// `forge decisions` and `forge show` display it beside supervisor
/// rulings), and emits `Event::TaskWithdrawn`. Unlike `forge retry`,
/// which carries dependents forward onto the new task, a withdrawn task
/// creates nothing to carry them onto: its dependents simply block, the
/// same path a failed or unverified task's dependents take (see
/// `Store::block_dependents`). `by` is "operator" or a caller-chosen
/// name. Returns the decision id.
pub fn withdraw(f: &Forge, id: i64, reason: &str, by: &str) -> Result<i64> {
    let Some(old) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    if !matches!(old.state, TaskState::Blocked | TaskState::Queued) {
        bail!(
            "task {id} is {}; only a blocked or queued task is withdrawn (a running attempt might still finish, and a landed task is already merged{})",
            old.state.as_str(),
            if old.state == TaskState::Running && !f.store.live_siblings(id)?.is_empty() {
                "; it is a duplicate of a live sibling, so --abort may stop it"
            } else {
                ""
            }
        );
    }
    if !f.store.withdraw(id, reason)? {
        bail!("task {id} changed state before it could be withdrawn");
    }
    let question = if old.reason.is_empty() {
        old.task.clone()
    } else {
        old.reason.clone()
    };
    let decision = f.store.insert_decision_by(crate::store::InsertDecisionBy {
        task_id: id,
        repo: &old.repo,
        question: &question,
        answer: reason,
        answered_by: by,
        citations: "",
        answered_for: old.question_to.as_deref(),
    })?;
    f.store.set_decision_retry(decision, id)?;
    f.report.emit(id, Event::TaskWithdrawn { reason });
    if let Some(iid) = old.initiative {
        crate::view::maybe_settle_initiative(f, id, iid)?;
    }
    // A dependent already blocked on this task (its after list re-pointed
    // here while it waited) is released now that this one is terminal;
    // one still queued is picked up by `block_dependents` instead.
    for d in f.store.release_dependents_of(id)? {
        f.report.emit(
            d,
            Event::Note {
                text: "unblocked: its dependencies landed or were withdrawn",
            },
        );
    }
    Ok(decision)
}

/// A task that landed settles the earlier tasks of its lineage that its
/// own retry chain answered: one still blocked, whose decision (the
/// kernel's demotion-as-task, or a supervisor answer) re-queued the next
/// task on the path down to this one, is withdrawn as `superseded by
/// <id>`, by `forge`, as a decision row. A blocked task whose follow-up
/// has not landed is left alone. Returns the ids withdrawn.
pub fn settle_superseded(f: &Forge, landed: i64) -> Result<Vec<i64>> {
    let mut settled = Vec::new();
    let mut child = landed;
    while let Some(parent) = f.store.task(child)?.and_then(|t| t.retry_of) {
        let Some(old) = f.store.task(parent)? else {
            break;
        };
        if old.state == TaskState::Blocked
            && f.store
                .decisions_in_lineage(parent)?
                .iter()
                .any(|d| d.task_id == Some(parent) && d.retry_id == Some(child))
        {
            withdraw(f, parent, &format!("superseded by {landed}"), "forge")?;
            settled.push(parent);
        }
        child = parent;
    }
    Ok(settled)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_shape_counts_length_paths_and_declared_checks() {
        let resolved = workflows::Resolved::default();
        let shape = task_shape("fix src/queue.rs and src/store/mod.rs please", &resolved, 3);
        assert_eq!(
            shape.text_len,
            "fix src/queue.rs and src/store/mod.rs please"
                .chars()
                .count() as i64
        );
        assert_eq!(shape.path_tokens, 2, "a text naming two paths counts two");
        assert!(!shape.tdd, "no steps at all resolved");
        assert_eq!(shape.declared_checks, 3);
    }

    #[test]
    fn task_shape_flags_a_tdd_workflow_but_not_direct() {
        let dir = tempfile::tempdir().unwrap();
        let tdd = workflows::resolve(dir.path(), "tdd").unwrap();
        assert!(
            task_shape("add a feature", &tdd, 0).tdd,
            "a tdd workflow flags hidden tests"
        );
        let direct = workflows::resolve(dir.path(), "direct").unwrap();
        assert!(!task_shape("add a feature", &direct, 0).tdd);
    }

    pub(super) fn fixture_forge() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::ctx::Paths {
            worktrees: dir.path().join("worktrees"),
            logs: dir.path().join("logs"),
            home: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = crate::store::Store::open(&dir.path().join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    /// The routing record (docs/ECONOMIST.md, "The routing record"): a
    /// task that named its own `--provider` routes every role under it,
    /// so `Task::routing` should record source `"flag"` for every one —
    /// never the project's or the operator's, even when both name a
    /// provider for the same role. `effective_provider_routed` is exactly
    /// what `engine::run_directive_step` and `assess::try_run` call to
    /// build each role's entry, so exercising it here covers what they'd
    /// record without needing a real attempt.
    #[test]
    fn a_task_flagged_with_provider_records_source_flag_for_every_role() {
        let (_dir, mut f) = fixture_forge();
        f.providers.insert(
            "devhome".to_string(),
            crate::agent::Provider {
                name: "devhome".to_string(),
                ..crate::agent::Provider::default()
            },
        );
        // Both layers name a provider for "code" too, so a pass here
        // proves the flag actually wins rather than merely being present.
        f.roles.insert("code".to_string(), "anthropic".to_string());
        f.store
            .create_project(&crate::store::Project {
                name: "acme".to_string(),
                ..Default::default()
            })
            .unwrap();
        f.store
            .set_project_defaults(
                "acme",
                &crate::store::ProjectDefaults {
                    role_providers: [("code".to_string(), "anthropic".to_string())].into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let t = Task {
            provider: "devhome".to_string(),
            project: Some("acme".to_string()),
            ..Default::default()
        };
        for role in ["code", "tests", "review", "plan", "assess"] {
            let (provider, source) = f.effective_provider_routed(&t, role).unwrap();
            assert_eq!(provider.name, "devhome", "role {role}");
            assert_eq!(source, "flag", "role {role}");
        }
    }

    /// The other half: a task that names no `--provider` resolves each
    /// role through its project's `[roles]` table when that role is
    /// there, else the operator's (`build_roles` always names every role,
    /// "anthropic" where the operator's own config names none — so
    /// "operator" is what a role with no project override always records
    /// through a real `Forge`; "default" only shows up against a
    /// hand-built, incomplete operator table, as `ctx::resolve_provider`'s
    /// own tests use).
    #[test]
    fn a_task_without_a_provider_flag_records_project_or_operator() {
        let (_dir, f) = fixture_forge();
        f.store
            .create_project(&crate::store::Project {
                name: "acme".to_string(),
                ..Default::default()
            })
            .unwrap();
        f.store
            .set_project_defaults(
                "acme",
                &crate::store::ProjectDefaults {
                    role_providers: [("code".to_string(), "anthropic".to_string())].into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let t = Task {
            provider: String::new(),
            project: Some("acme".to_string()),
            ..Default::default()
        };
        let (_, code_source) = f.effective_provider_routed(&t, "code").unwrap();
        assert_eq!(code_source, "project", "the project's [roles] names code");
        let (_, tests_source) = f.effective_provider_routed(&t, "tests").unwrap();
        assert_eq!(
            tests_source, "operator",
            "the project names no tests provider, so the operator's table applies"
        );
    }
}
