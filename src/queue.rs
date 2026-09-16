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
/// negatives (`no_land`, `no_context`) are the operator's control arms
/// and default to off.
#[derive(Debug, Clone, Default)]
pub struct TaskRequest {
    pub repo: PathBuf,
    pub task: String,
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
    /// Whether the request said `--journal` (`Some(true)`) or
    /// `--no-journal` (`Some(false)`) itself; `None` when it said
    /// neither, leaving the arm to the operator's control fraction (see
    /// `assign_journal_arm`).
    pub journal_choice: Option<bool>,
    pub no_context: bool,
    pub resume_on_failure: bool,
}

/// The task's journal flag and how it got that value. An explicit
/// `--journal`/`--no-journal` always wins and records `"explicit"`;
/// otherwise a deterministic draw from the task id assigns `"control"`
/// (journal off) with probability `fraction`, else `"treatment"`. See
/// docs/LATER.md, "The journal measurement was ill-posed three times".
fn assign_journal_arm(id: i64, choice: Option<bool>, fraction: f64) -> (bool, &'static str) {
    match choice {
        Some(on) => (on, "explicit"),
        None if journal_control_draw(id, fraction) => (false, "control"),
        None => (true, "treatment"),
    }
}

/// Whether `id` draws into the control arm at `fraction`: a pure function
/// of the two, so the assignment is reproducible from the id alone and
/// never needs to be persisted separately from the id it came from.
/// `fraction <= 0.0` never draws control; `fraction >= 1.0` always does.
fn journal_control_draw(id: i64, fraction: f64) -> bool {
    if fraction <= 0.0 {
        return false;
    }
    // A splitmix64-style finalizer: built to take a small sequential
    // counter (task ids) to well-spread output, unlike a plain multiply.
    let mut x = id as u64;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    let draw = (x % 1_000_000) as f64 / 1_000_000.0;
    draw < fraction
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
    let workflow = args
        .workflow
        .clone()
        .or_else(|| project.as_ref().and_then(|p| p.workflow.clone()))
        .unwrap_or_else(|| "direct".to_string());
    let cfg = config::load_working(&repo).await?;
    let wf = workflows::get(&f.paths.home, &workflow)?
        .with_context(|| format!("unknown workflow {workflow:?}; see `forge workflows`"))?;
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
    let resolved = workflows::resolve(&f.paths.home, &workflow)?;
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
    let code_provider = crate::ctx::resolve_provider(
        &f.providers,
        &f.roles,
        &project_roles,
        &provider_name,
        "code",
    )?;
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| code_provider.model.clone().unwrap_or_default());
    let mut t = Task {
        repo: repo.display().to_string(),
        task: args.task.clone(),
        base_branch: cfg.base_branch.clone(),
        model,
        provider: provider_name,
        max_turns: args.max_turns as i64,
        max_attempts: args.retries as i64 + 1,
        timeout_secs: args.timeout_secs as i64,
        checks: args.checks.clone(),
        state: TaskState::Queued,
        created_at: unix_now(),
        budget_usd: args.budget,
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
    let (journal, arm) = assign_journal_arm(t.id, args.journal_choice, f.measure.journal_control);
    t.journal = journal;
    t.journal_arm = arm.to_string();
    f.store.update_task(&t)?;
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

/// One task parsed from an initiative's `--from` file: a paragraph, its
/// optional dependency on an earlier paragraph (1-based, within the
/// file), its optional repository override, and its text.
pub struct FileTask {
    pub after: Option<usize>,
    pub repo: Option<String>,
    pub text: String,
}

/// Parse an initiative's task file: one task per paragraph (blank-line
/// separated), each optionally led by an `after: <n>` line naming an
/// earlier paragraph in the file as a dependency and a `repo: <path>`
/// line naming the repository it runs against instead of the project's
/// first one (see docs/PROJECTS.md, "Verbs"). Both lead lines may appear,
/// in either order; whatever is left is the task's text.
pub fn parse_initiative_file(text: &str) -> Result<Vec<FileTask>> {
    let mut out: Vec<FileTask> = Vec::new();
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        // 1-based, and counted only over paragraphs that hold a task, so
        // it matches the position an `after:` line in a later paragraph
        // means to name.
        let this_no = out.len() + 1;
        let mut after = None;
        let mut repo = None;
        let mut body: Vec<&str> = Vec::new();
        let mut in_lead = true;
        for line in para.lines() {
            if in_lead && let Some(n) = line.strip_prefix("after:") {
                let n: usize = n.trim().parse().with_context(|| {
                    format!("paragraph {this_no}: `after:` needs a paragraph number")
                })?;
                if n == 0 || n >= this_no {
                    bail!(
                        "paragraph {this_no}: `after: {n}` does not name an earlier paragraph in this file"
                    );
                }
                after = Some(n);
                continue;
            }
            if in_lead && let Some(p) = line.strip_prefix("repo:") {
                repo = Some(p.trim().to_string());
                continue;
            }
            in_lead = false;
            body.push(line);
        }
        let body = body.join("\n").trim().to_string();
        if body.is_empty() {
            bail!("paragraph {this_no} has no task text");
        }
        out.push(FileTask {
            after,
            repo,
            text: body,
        });
    }
    Ok(out)
}

/// Split a task's recorded plan into items: paragraphs (blank-line
/// separated), trimmed, empties dropped. A plan is prose from the
/// investigate directive rather than a delimited list, so this uses the
/// same split `parse_initiative_file` gives a hand-written file.
pub fn plan_items(text: &str) -> Vec<String> {
    text.split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// File a task's recorded plan into an initiative: one task per plan
/// item, in order, each depending on the previous, against the
/// originating task's repository, with the originating task recorded as
/// a reference of kind `plan` on each (see docs/PROJECTS.md, "Tasks").
/// Returns the new tasks' ids, in order. The caller checks the plan is
/// non-empty; called only once one is known to exist.
pub async fn file_plan(f: &Forge, origin: &Task, initiative: i64) -> Result<Vec<i64>> {
    let items = plan_items(&origin.plan);
    let mut ids: Vec<i64> = Vec::new();
    for item in &items {
        let req = TaskRequest {
            repo: PathBuf::from(&origin.repo),
            task: item.clone(),
            model: Some("sonnet".to_string()),
            max_turns: 100,
            retries: 1,
            timeout_secs: 1800,
            after: ids.last().copied().into_iter().collect(),
            initiative: Some(initiative),
            ..Default::default()
        };
        let t = enqueue(f, &req, None).await?;
        f.store.insert_task_ref(
            t.id,
            "plan",
            &format!("forge://task/{}", origin.id),
            "",
            "operator",
        )?;
        ids.push(t.id);
    }
    Ok(ids)
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
        model: Some(t.model.clone()),
        // Empty means the original task named no `--provider` and
        // resolved per role; a retry should resolve the same way, not
        // pin whatever "code" happened to pick at the time.
        provider: (!t.provider.is_empty()).then(|| t.provider.clone()),
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
        .find(|a| a.is_agent());
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
            "task {id} is {}; only a blocked or queued task is withdrawn (a running attempt might still finish, and a landed task is already merged)",
            old.state.as_str()
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
    let decision = f
        .store
        .insert_decision_by(id, &old.repo, &question, reason, by, "")?;
    f.store.set_decision_retry(decision, id)?;
    f.report.emit(id, Event::TaskWithdrawn { reason });
    if let Some(iid) = old.initiative {
        crate::view::maybe_settle_initiative(f, id, iid)?;
    }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_control_draw_is_a_pure_function_of_id_and_fraction() {
        // Same inputs, called independently, always agree: nothing but
        // (id, fraction) feeds the draw.
        for id in [1, 2, 3, 42, 1_000, 1_000_000] {
            for frac in [0.0, 0.1, 0.3, 0.5, 0.9, 1.0] {
                let a = journal_control_draw(id, frac);
                let b = journal_control_draw(id, frac);
                assert_eq!(a, b, "id {id} fraction {frac} disagreed with itself");
            }
        }
        // Raising the fraction only adds control assignments: each id's
        // draw is fixed and only the threshold moves, so the set of ids
        // assigned control at a lower fraction is a subset of a higher one.
        let ids: Vec<i64> = (1..5_000).collect();
        let lo: Vec<bool> = ids
            .iter()
            .map(|&id| journal_control_draw(id, 0.2))
            .collect();
        let hi: Vec<bool> = ids
            .iter()
            .map(|&id| journal_control_draw(id, 0.6))
            .collect();
        for (l, h) in lo.iter().zip(hi.iter()) {
            assert!(!l || *h, "raising the fraction dropped a control draw");
        }
    }

    #[test]
    fn parse_initiative_file_reads_after_and_repo_lines_and_leaves_the_rest_as_text() {
        let tasks = parse_initiative_file(
            "repo: /a\nfirst task\nsecond line\n\nafter: 1\nsecond task\n\nafter: 1\nrepo: /b\nthird task",
        )
        .unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].repo.as_deref(), Some("/a"));
        assert_eq!(tasks[0].after, None);
        assert_eq!(tasks[0].text, "first task\nsecond line");
        assert_eq!(tasks[1].repo, None);
        assert_eq!(tasks[1].after, Some(1));
        assert_eq!(tasks[1].text, "second task");
        assert_eq!(tasks[2].repo.as_deref(), Some("/b"));
        assert_eq!(tasks[2].after, Some(1));
        assert_eq!(tasks[2].text, "third task");
    }

    #[test]
    fn plan_items_splits_on_blank_lines_and_drops_empties() {
        let items = plan_items("first item\nmore of it\n\n\nsecond item\n\nthird item\n");
        assert_eq!(
            items,
            vec!["first item\nmore of it", "second item", "third item"]
        );
        assert_eq!(plan_items("  \n\n  "), Vec::<String>::new());
    }

    #[test]
    fn parse_initiative_file_refuses_after_that_names_itself_or_the_future() {
        assert!(parse_initiative_file("after: 1\nonly task").is_err());
        assert!(parse_initiative_file("first task\n\nafter: 2\nsecond task").is_err());
    }

    #[test]
    fn a_fraction_of_zero_never_assigns_control() {
        for id in 1..10_000 {
            assert!(
                !journal_control_draw(id, 0.0),
                "id {id} drew control at fraction 0.0"
            );
        }
    }
}
