use super::task_records::{LogArgs, log, show, trace};
use super::*;
use crate::landing::land_task;

#[derive(Subcommand)]
pub(super) enum TaskCmd {
    /// Change a queued or blocked task's spec in place, replacing only
    /// the fields given; refused when none are, and refused on a running
    /// or finished task (only its next attempt, or a retry, can change
    /// those). Recorded as a decision on the task naming each field's
    /// old and new value, so the change is on the record; the queue's
    /// next claim reads the new values. Prints the task afterward.
    Set {
        id: i64,
        /// Cost cap for this task in USD
        #[arg(long)]
        budget: Option<f64>,
        /// Turns per attempt
        #[arg(long = "max-turns")]
        max_turns: Option<u32>,
        /// Wall-clock limit per attempt in seconds
        #[arg(long)]
        timeout_secs: Option<u32>,
        /// Extra attempts after a failure, each fed the previous failure
        #[arg(long)]
        retries: Option<u32>,
        /// Replace the task text
        #[arg(long, conflicts_with = "text_file")]
        text: Option<String>,
        /// Replace the task text with this file's contents
        #[arg(long = "text-file")]
        text_file: Option<PathBuf>,
        /// Replace the workflow (must exist and fit the repository)
        #[arg(long)]
        workflow: Option<String>,
        /// Replace the tasks this one waits on (repeatable)
        #[arg(long, conflicts_with = "no_after")]
        after: Vec<i64>,
        /// Wait on nothing
        #[arg(long = "no-after")]
        no_after: bool,
        /// Replace the task's own acceptance commands (repeatable)
        #[arg(long = "check", conflicts_with = "no_checks")]
        checks: Vec<String>,
        /// Drop the task's own acceptance commands
        #[arg(long = "no-checks")]
        no_checks: bool,
    },
}

async fn enqueue(f: &Forge, args: &TaskArgs) -> Result<Task> {
    crate::queue::enqueue(f, &args.into(), None).await
}

/// Answer a task blocked on a question. `by` is "operator" by default, or
/// who else the answer came from (a channel plugin's contact name).
/// `project`, when given, scopes the answer to that project and to `by`
/// as the question's recipient (see `queue::check_answer_scope`); unset,
/// as the operator's own `forge answer` always leaves it, the answer
/// reaches any task.
async fn answer(id: i64, text: String, by: String, project: Option<String>) -> Result<()> {
    let f = Forge::open(false, false)?;
    if let Some(t) = f.store.task(id)?
        && t.state == TaskState::Blocked
        && t.proposal_json.is_some()
    {
        return match crate::concierge::answer_proposal(&f, id, &text, &by).await? {
            crate::concierge::ProposalAnswered::Initiative { initiative, tasks } => {
                out!(
                    "answered task {id}: filed initiative {initiative} ({} task(s))",
                    tasks.len()
                );
                Ok(())
            }
            crate::concierge::ProposalAnswered::Declined => {
                out!("answered task {id}: proposal declined");
                Ok(())
            }
        };
    }
    let scope = project.as_deref().map(|p| (p, by.as_str()));
    let (_, n) = crate::queue::answer(&f, id, &text, &by, "", scope).await?;
    out!("answered task {id} as {}", n.id);
    Ok(())
}

/// Withdraw a blocked or queued task, as the operator.
fn withdraw(id: i64, reason: String, by: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    crate::queue::withdraw(&f, id, &reason, &by)?;
    out!("withdrew task {id}: {reason}");
    Ok(())
}

async fn run(args: TaskArgs) -> Result<()> {
    let f = Arc::new(Forge::open(true, false)?);
    if let Some(msg) = worker::day_budget_reached(&f)? {
        bail!("{msg}");
    }
    let t = enqueue(&f, &args).await?;
    if !f.store.claim(t.id, std::process::id() as i64)? {
        bail!(
            "task {} was claimed by another worker before this one could start it",
            t.id
        );
    }
    eprintln!("task     {}", t.id);
    if worker::drive(f, t.id).await? != TaskState::Succeeded {
        std::process::exit(1);
    }
    Ok(())
}

async fn retry(id: i64, chain: bool, o: crate::queue::RetryOverrides) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(old) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    if matches!(old.state, TaskState::Queued | TaskState::Running) {
        bail!(
            "task {id} is {}; only a finished task is retried",
            old.state.as_str()
        );
    }
    let mut made: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut queue = vec![old];
    let mut first = true;
    while !queue.is_empty() {
        let t = queue.remove(0);
        if made.contains_key(&t.id) {
            continue;
        }
        let after = t
            .after
            .iter()
            .map(|&d| crate::queue::map_dep(&f, d, &made))
            .collect::<Result<Vec<_>>>()?;
        let args = crate::queue::retry_request(&t, &o, first, after, None);
        let n = crate::queue::enqueue(&f, &args, Some(t.id)).await?;
        out!(
            "retried task {} as {}{}",
            t.id,
            n.id,
            if n.after.is_empty() {
                String::new()
            } else {
                format!(
                    " (after {})",
                    n.after
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
        made.insert(t.id, n.id);
        // Dependents follow a retry on their own now (enqueue_with reroutes
        // them); `--chain` is kept for callers that still pass it.
        let _ = chain;
        first = false;
    }
    out!("{} queued", f.store.queued_count()?);
    Ok(())
}

async fn supervise_now(id: i64) -> Result<()> {
    let f = Forge::open(true, false)?;
    match crate::supervisor::supervise(&f, id).await? {
        crate::supervisor::Ruled::Answered { retry } => out!("answered; re-queued as task {retry}"),
        crate::supervisor::Ruled::Prerequisite {
            prerequisite,
            retry,
        } => out!("filed prerequisite task {prerequisite}; re-queued as task {retry} behind it"),
        crate::supervisor::Ruled::Superseded { by } => out!("superseded by task {by}"),
        crate::supervisor::Ruled::Accepted { landed } => out!("accepted the branch: {landed}"),
        crate::supervisor::Ruled::Escalated(why) => out!("escalated: {why}"),
        crate::supervisor::Ruled::Skipped(why) => out!("skipped: {why}"),
    }
    Ok(())
}

fn decisions(
    repo: Option<PathBuf>,
    project: Option<String>,
    initiative: Option<i64>,
    grep: Option<String>,
    json: bool,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let repo = repo
        .map(|p| p.canonicalize().context("repo path"))
        .transpose()?
        .map(|p| p.display().to_string());
    let rows: Vec<crate::view::DecisionRow> = f
        .store
        .decisions(&crate::store::DecisionFilter {
            repo,
            project,
            initiative,
            grep,
        })?
        .iter()
        .map(|d| {
            let outcome = d
                .retry_id
                .and_then(|r| f.store.task(r).ok().flatten())
                .map(|t| t.state);
            crate::view::DecisionRow::new(d, outcome)
        })
        .collect();
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no decisions");
        return Ok(());
    }
    for d in &rows {
        let outcome = d
            .outcome
            .as_ref()
            .map(|s| format!(" → task {} {}", d.retry_id.unwrap_or_default(), s))
            .unwrap_or_default();
        let task_col = d
            .task_id
            .map(|t| t.to_string())
            .unwrap_or_else(|| "-".to_string());
        out!("{:<5} task {:<5} Q: {}", d.id, task_col, d.question);
        out!(
            "{:<17}A ({}{}): {}{}",
            "",
            d.answered_by,
            if d.citations.is_empty() {
                String::new()
            } else {
                format!(", citing {}", d.citations)
            },
            d.answer,
            outcome
        );
    }
    Ok(())
}

async fn task_set(id: i64, edit: crate::queue::TaskEdit) -> Result<()> {
    let f = Forge::open(false, false)?;
    crate::queue::edit_task(&f, id, &edit).await?;
    show(id, false)
}

async fn add(args: TaskArgs, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let t = enqueue(&f, &args).await?;
    let queued = f.store.queued_count()?;
    if json {
        out!("{}", serde_json::json!({ "id": t.id, "queued": queued }));
    } else {
        out!("queued task {} ({queued} queued)", t.id);
    }
    Ok(())
}

fn journal(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    if json {
        let entries = crate::journal::entries_for(&f, &t).map_err(|e| match e {
            crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
        })?;
        out!("{}", serde_json::to_string(&entries)?);
        return Ok(());
    }
    let j = crate::journal::journal_for(&f, &t).map_err(|e| match e {
        crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
    })?;
    if j.is_empty() {
        out!("nothing ran before task {id} in its piece of work");
    } else {
        out!("{j}");
    }
    Ok(())
}

/// The integrator, by hand, for a task that verified but did not land: a
/// task from before the repository had a remote, or one queued --no-land
/// that a human has now cleared.
async fn land(id: i64) -> Result<()> {
    let f = Forge::open(true, true)?;
    let line = land_task(&f, id, true).await?;
    out!("{line}");
    Ok(())
}

/// The integrator's merge-and-verify half, by hand, for a repository that
/// keeps a human at the gate: each task's branch merged onto the base in
/// order, every check with every hidden suite after each, the result left
/// as a branch. A conflict or a red check stops it and says which.
async fn integrate(ids: Vec<i64>) -> Result<()> {
    let f = Forge::open(true, false)?;
    let report = crate::landing::integrate_many(&f, &ids).await?;
    out!("{}", report.render());
    match report.outcome {
        crate::landing::IntegrateOutcome::Ready => Ok(()),
        crate::landing::IntegrateOutcome::Conflict { task_id, files, .. } => bail!(
            "task {task_id} conflicts with what came before it: {}",
            files.join(", ")
        ),
        crate::landing::IntegrateOutcome::VerifyFailed {
            task_id, reason, ..
        } => bail!("the tree with task {task_id} merged does not verify: {reason}"),
    }
}

async fn dispatch_run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Run(args) => run(args).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_add(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Add { args, json } => add(args, json).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_work(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Work {
            jobs,
            poll,
            once,
            max_tasks,
        } => {
            let f = Arc::new(Forge::open(true, jobs > 1)?);
            worker::work(
                f,
                worker::WorkOpts {
                    jobs,
                    poll: (!once).then_some(poll),
                    max_tasks,
                },
            )
            .await
        }
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_log(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Log {
            limit,
            json,
            state,
            repo,
            before,
            grep,
            workflow,
            project,
            initiative,
            touches,
            touches_text,
            failed_on,
            reason,
        } => log(
            LogArgs {
                limit,
                state,
                repo,
                before,
                grep,
                workflow,
                project,
                initiative,
                touches,
                touches_text,
                failed_on,
                reason,
            },
            json,
        ),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_retry(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Retry {
            id,
            chain,
            retries,
            budget,
            max_turns,
            timeout_secs,
            workflow,
        } => {
            retry(
                id,
                chain,
                crate::queue::RetryOverrides {
                    retries,
                    budget,
                    max_turns,
                    timeout_secs,
                    workflow,
                },
            )
            .await
        }
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_answer(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Answer {
            id,
            text,
            by,
            project,
        } => answer(id, text, by, project).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_withdraw(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Withdraw { id, reason, by } => withdraw(id, reason, by),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_decisions(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Decisions {
            repo,
            project,
            initiative,
            grep,
            json,
        } => decisions(repo, project, initiative, grep, json),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_show(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Show { id, json } => show(id, json),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_supervise(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Supervise { id } => supervise_now(id).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_trace(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Trace { id, json } => trace(id, json),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_integrate(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Integrate { ids } => integrate(ids).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_land(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Land { id } => land(id).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_journal(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Journal { id, json } => journal(id, json),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_task(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Task { cmd } => match cmd {
            TaskCmd::Set {
                id,
                budget,
                max_turns,
                timeout_secs,
                retries,
                text,
                text_file,
                workflow,
                after,
                no_after,
                checks,
                no_checks,
            } => {
                let text = match text_file {
                    Some(p) => Some(
                        std::fs::read_to_string(&p)
                            .with_context(|| format!("--text-file {}", p.display()))?,
                    ),
                    None => text,
                };
                task_set(
                    id,
                    crate::queue::TaskEdit {
                        budget,
                        max_turns,
                        timeout_secs,
                        retries,
                        text,
                        workflow,
                        after: (no_after || !after.is_empty()).then_some(after),
                        checks: (no_checks || !checks.is_empty()).then_some(checks),
                    },
                )
                .await
            }
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Run(..) => dispatch_run(cmd).await,
        Cmd::Add { .. } => dispatch_add(cmd).await,
        Cmd::Work { .. } => dispatch_work(cmd).await,
        Cmd::Log { .. } => dispatch_log(cmd).await,
        Cmd::Retry { .. } => dispatch_retry(cmd).await,
        Cmd::Answer { .. } => dispatch_answer(cmd).await,
        Cmd::Withdraw { .. } => dispatch_withdraw(cmd).await,
        Cmd::Decisions { .. } => dispatch_decisions(cmd).await,
        Cmd::Show { .. } => dispatch_show(cmd).await,
        Cmd::Supervise { .. } => dispatch_supervise(cmd).await,
        Cmd::Trace { .. } => dispatch_trace(cmd).await,
        Cmd::Integrate { .. } => dispatch_integrate(cmd).await,
        Cmd::Land { .. } => dispatch_land(cmd).await,
        Cmd::Journal { .. } => dispatch_journal(cmd).await,
        Cmd::Task { .. } => dispatch_task(cmd).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}
