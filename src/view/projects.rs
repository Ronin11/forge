use super::tasks::request_kind;
use crate::ctx::Forge;
use crate::store::{Task, TaskState};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One repository listed under a project, and the paths it owns there
/// (`None` scope means the whole repository).
#[derive(Serialize)]
pub struct ProjectRepoRow {
    pub repo: String,
    pub scope: Option<String>,
}

impl From<&crate::store::ProjectRepo> for ProjectRepoRow {
    fn from(r: &crate::store::ProjectRepo) -> Self {
        ProjectRepoRow {
            repo: r.repo.clone(),
            scope: r.scope.clone(),
        }
    }
}

/// One row of `forge project list --json` / `forge project show --json`:
/// a project, its repositories and their scopes, task counts by state,
/// cost, and its own defaults (see docs/PROJECTS.md, "Defaults").
#[derive(Serialize)]
pub struct ProjectRow {
    pub name: String,
    pub purpose: String,
    pub created_at: i64,
    pub repos: Vec<ProjectRepoRow>,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub unverified: i64,
    pub blocked: i64,
    pub withdrawn: i64,
    pub cost_usd: f64,
    /// Jobs (`kind = "run"` workflow runs) started in the last rolling
    /// 24h, counted separately from the task rollup above (docs/JOBS.md
    /// step 1d): every one of them, then how many of those reached each
    /// terminal state.
    pub jobs_today: i64,
    pub jobs_ok: i64,
    pub jobs_failed: i64,
    pub jobs_needs_human: i64,
    pub jobs_skipped: i64,
    pub workflow: Option<String>,
    pub per_task_usd: Option<f64>,
    pub per_initiative_usd: Option<f64>,
    pub supervisor_model: Option<String>,
    pub supervisor_per_lineage: Option<i64>,
    pub protected: Vec<String>,
    pub role_providers: std::collections::BTreeMap<String, String>,
    /// The escalator's proposals made on this project, newest first, and
    /// how each was answered (see docs/INTAKE.md, "The escalator").
    pub proposals: Vec<ProposalRow>,
}

/// A project's purpose as shown to a person: empty, rather than the
/// migration's placeholder, when nobody has set a real one yet (see
/// `crate::store::is_placeholder_purpose`).
fn real_purpose(purpose: &str) -> String {
    if crate::store::is_placeholder_purpose(purpose) {
        String::new()
    } else {
        purpose.to_string()
    }
}

pub fn project_row(f: &Forge, p: &crate::store::Project) -> Result<ProjectRow> {
    let repos = f
        .store
        .project_repos(&p.name)?
        .iter()
        .map(ProjectRepoRow::from)
        .collect();
    let cost = f.store.project_task_stats(&p.name)?.cost;
    let job_stats = f
        .store
        .project_job_stats(&p.name, crate::unix_now() - 86_400)?;
    let tasks = f.store.project_tasks(&p.name)?;
    let mut proposals: Vec<ProposalRow> = tasks.iter().filter_map(proposal_row).collect();
    proposals.sort_by(|a, b| b.task_id.cmp(&a.task_id));
    let mut stats = crate::store::ProjectTaskStats::default();
    for (t, _) in latest_per_lineage(f, &tasks)? {
        match t.state {
            TaskState::Queued => stats.queued += 1,
            TaskState::Running => stats.running += 1,
            TaskState::Succeeded => stats.succeeded += 1,
            TaskState::Failed => stats.failed += 1,
            TaskState::Unverified => stats.unverified += 1,
            TaskState::Blocked => stats.blocked += 1,
            TaskState::Withdrawn => stats.withdrawn += 1,
        }
    }
    Ok(ProjectRow {
        name: p.name.clone(),
        purpose: real_purpose(&p.purpose),
        created_at: p.created_at,
        repos,
        queued: stats.queued,
        running: stats.running,
        succeeded: stats.succeeded,
        failed: stats.failed,
        unverified: stats.unverified,
        blocked: stats.blocked,
        withdrawn: stats.withdrawn,
        cost_usd: cost,
        jobs_today: job_stats.today,
        jobs_ok: job_stats.ok,
        jobs_failed: job_stats.failed,
        jobs_needs_human: job_stats.needs_human,
        jobs_skipped: job_stats.skipped,
        workflow: p.workflow.clone(),
        per_task_usd: p.per_task_usd,
        per_initiative_usd: p.per_initiative_usd,
        supervisor_model: p.supervisor_model.clone(),
        supervisor_per_lineage: p.supervisor_per_lineage,
        protected: p.protected.clone().unwrap_or_default(),
        role_providers: p.role_providers.clone(),
        proposals,
    })
}

/// Every project, alphabetically, as `forge project list` shows it.
pub fn project_rows(f: &Forge) -> Result<Vec<ProjectRow>> {
    f.store
        .list_projects()?
        .iter()
        .map(|p| project_row(f, p))
        .collect()
}

/// The L0 rule name(s) a failed task's `reason` blames, in the exact form
/// `engine::l0_failure_reason` writes it ("L0 failed: has-commits",
/// optionally followed by " (after N attempt(s))"). `None` for any other
/// kind of failure: an agent failure, a budget cap, or a failing L1/L2
/// check never sets this prefix.
/// The L0 rules a failed task's reason names: "L0 failed: has-commits,
/// changes-match-git (after 2 attempt(s))" names two. A task can fail
/// several at once, and a streak on one of them must not be broken by a
/// failure that names it among others (initiative 2's third has-commits
/// failure also named changes-match-git and reset the count).
fn l0_rules_of(reason: &str) -> Vec<String> {
    let Some(rest) = reason.strip_prefix("L0 failed: ") else {
        return Vec::new();
    };
    let rest = rest.split(" (after").next().unwrap_or(rest).trim();
    rest.split(',')
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect()
}

/// The trailing run of failed tasks that all name one L0 rule, over
/// terminal tasks in the order they finished: the rule and the run's
/// length. Any terminal task that did not fail on an L0 rule ends the run.
pub(crate) fn same_rule_streak(terminal: &[(TaskState, &str)]) -> Option<(String, i64)> {
    let mut rule: Option<String> = None;
    let mut len = 0i64;
    for (state, reason) in terminal {
        let rules = if *state == TaskState::Failed {
            l0_rules_of(reason)
        } else {
            Vec::new()
        };
        if rules.is_empty() {
            rule = None;
            len = 0;
        } else if let Some(r) = &rule
            && rules.iter().any(|x| x == r)
        {
            len += 1;
        } else {
            rule = Some(rules[0].clone());
            len = 1;
        }
    }
    rule.map(|r| (r, len))
}

/// `tasks`, collapsed to one entry per lineage: a task and every task
/// that retries it, directly or through further retries, contribute only
/// their latest task (the one nothing in the lineage retries), since a
/// lineage's fate is its latest task's even though every task in it was
/// worked and paid for. Groups by `Store::root_of`, which walks the same
/// `retry_of` chain as `lineage_ids`. Paired with each latest task is how
/// many earlier tasks came before it in the lineage (its retry count).
fn latest_per_lineage(f: &Forge, tasks: &[Task]) -> Result<Vec<(Task, i64)>> {
    let mut groups: std::collections::BTreeMap<i64, Vec<Task>> = Default::default();
    for t in tasks {
        let root = f.store.root_of(t.id)?;
        groups.entry(root).or_default().push(t.clone());
    }
    Ok(groups
        .into_values()
        .map(|mut lineage| {
            lineage.sort_by_key(|t| t.id);
            let retries = (lineage.len() - 1) as i64;
            (lineage.pop().unwrap(), retries)
        })
        .collect())
}

/// One line, exactly as docs/PROJECTS.md, "State" derives it: "open"
/// while any task is queued or running; "held" when that is also true and
/// the worker is holding new claims for the initiative; "done with
/// failures" when none remain open and some failed; "done" otherwise.
pub fn initiative_state(tasks: &[Task], hold: Option<&str>) -> &'static str {
    let any_open = tasks.iter().any(|t| {
        !matches!(
            t.state,
            TaskState::Succeeded | TaskState::Failed | TaskState::Unverified | TaskState::Withdrawn
        )
    });
    if any_open {
        return if hold.is_some() { "held" } else { "open" };
    }
    if tasks.iter().any(|t| t.state == TaskState::Failed) {
        "done with failures"
    } else {
        "done"
    }
}

/// `initiative_hold`'s full answer: the short tag it exposes as
/// `held_rule` ("budget" or the rule name) paired with a human-readable
/// detail of the same fact (the amounts, or the rule and its streak) —
/// what `forge doctor`'s `initiatives` check reports, since `held_rule`
/// alone does not say how close or by how much.
fn initiative_hold_detail(
    f: &Forge,
    ini: &crate::store::Initiative,
) -> Result<Option<(String, String)>> {
    let budget = ini.budget_usd.or_else(|| {
        f.store
            .project(&ini.project)
            .ok()
            .flatten()
            .and_then(|p| p.per_initiative_usd)
    });
    if let Some(b) = budget {
        let spent = f.store.initiative_cost(ini.id)?;
        if spent >= b {
            return Ok(Some((
                "budget".to_string(),
                format!("budget: ${spent:.2} of ${b:.2}"),
            )));
        }
    }
    if ini.stop_after_same_rule <= 0 {
        return Ok(None);
    }
    let tasks = f.store.initiative_tasks(ini.id)?;
    let mut terminal: Vec<&Task> = tasks
        .iter()
        .filter(|t| {
            matches!(
                t.state,
                TaskState::Succeeded
                    | TaskState::Failed
                    | TaskState::Unverified
                    | TaskState::Withdrawn
            )
        })
        .collect();
    terminal.sort_by_key(|t| t.finished_at.unwrap_or(0));
    let seq: Vec<(TaskState, &str)> = terminal
        .iter()
        .map(|t| (t.state, t.reason.as_str()))
        .collect();
    Ok(same_rule_streak(&seq)
        .filter(|(_, len)| *len >= ini.stop_after_same_rule)
        .map(|(rule, len)| {
            let detail = format!("stop rule: {rule} (streak {len})");
            (rule, detail)
        }))
}

/// `Some` when the worker is currently holding new claims for this
/// initiative (see docs/PROJECTS.md, "Stop rule and budget"): its summed
/// cost has reached its budget (reported as `"budget"`), or its trailing
/// run of failed tasks all blame the same L0 rule and that run has
/// reached `stop_after_same_rule` (reported as that rule's name).
pub fn initiative_hold(f: &Forge, ini: &crate::store::Initiative) -> Result<Option<String>> {
    Ok(initiative_hold_detail(f, ini)?.map(|(tag, _)| tag))
}

/// `initiative_hold`'s detail message alone, for `forge doctor`'s
/// `initiatives` check: "budget: $spent of $cap" or "stop rule: <rule>
/// (streak <n>)".
pub(crate) fn initiative_hold_reason(
    f: &Forge,
    ini: &crate::store::Initiative,
) -> Result<Option<String>> {
    Ok(initiative_hold_detail(f, ini)?.map(|(_, detail)| detail))
}

/// Settle an initiative once every one of its tasks has reached a
/// terminal state (succeeded, failed, unverified or withdrawn): record
/// `settled_at` and emit `Event::InitiativeSettled`, tagged with
/// `task_id`, the task whose own change completed it (see
/// docs/PROJECTS.md, "One notification and one report"). A no-op once
/// already settled, or while the initiative still has open work.
pub fn maybe_settle_initiative(f: &Forge, task_id: i64, initiative_id: i64) -> Result<()> {
    let Some(ini) = f.store.initiative(initiative_id)? else {
        return Ok(());
    };
    if ini.settled_at.is_some() {
        return Ok(());
    }
    let tasks = f.store.initiative_tasks(initiative_id)?;
    let latest: Vec<Task> = latest_per_lineage(f, &tasks)?
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    let all_terminal = latest.iter().all(|t| {
        matches!(
            t.state,
            TaskState::Succeeded | TaskState::Failed | TaskState::Unverified | TaskState::Withdrawn
        )
    });
    if !all_terminal {
        return Ok(());
    }
    if f.store
        .settle_initiative(initiative_id, crate::unix_now())?
    {
        let cost = f.store.initiative_cost(initiative_id)?;
        let state = initiative_state(&latest, None).to_string();
        f.report.emit(
            task_id,
            crate::report::Event::InitiativeSettled {
                id: initiative_id,
                state: &state,
                cost,
            },
        );
    }
    Ok(())
}

/// One row of `forge initiative list` / `--json` and `forge initiative
/// show`: an initiative, its derived state, task counts by state, cost
/// and its own settings.
#[derive(Serialize)]
pub struct InitiativeRow {
    pub id: i64,
    pub project: String,
    pub outcome: String,
    pub state: String,
    pub held_rule: Option<String>,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub unverified: i64,
    pub blocked: i64,
    pub withdrawn: i64,
    pub cost_usd: f64,
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: i64,
    pub created_at: i64,
    pub settled_at: Option<i64>,
}

pub fn initiative_row(f: &Forge, ini: &crate::store::Initiative) -> Result<InitiativeRow> {
    let tasks = f.store.initiative_tasks(ini.id)?;
    let latest: Vec<Task> = latest_per_lineage(f, &tasks)?
        .into_iter()
        .map(|(t, _)| t)
        .collect();
    let hold = initiative_hold(f, ini)?;
    let state = initiative_state(&latest, hold.as_deref()).to_string();
    let mut stats = crate::store::ProjectTaskStats::default();
    for t in &latest {
        match t.state {
            TaskState::Queued => stats.queued += 1,
            TaskState::Running => stats.running += 1,
            TaskState::Succeeded => stats.succeeded += 1,
            TaskState::Failed => stats.failed += 1,
            TaskState::Unverified => stats.unverified += 1,
            TaskState::Blocked => stats.blocked += 1,
            TaskState::Withdrawn => stats.withdrawn += 1,
        }
    }
    Ok(InitiativeRow {
        id: ini.id,
        project: ini.project.clone(),
        outcome: ini.outcome.clone(),
        state,
        held_rule: hold,
        queued: stats.queued,
        running: stats.running,
        succeeded: stats.succeeded,
        failed: stats.failed,
        unverified: stats.unverified,
        blocked: stats.blocked,
        withdrawn: stats.withdrawn,
        cost_usd: f.store.initiative_cost(ini.id)?,
        budget_usd: ini.budget_usd,
        stop_after_same_rule: ini.stop_after_same_rule,
        created_at: ini.created_at,
        settled_at: ini.settled_at,
    })
}

/// Every initiative, oldest first; only `project`'s when given.
pub fn initiative_rows(f: &Forge, project: Option<&str>) -> Result<Vec<InitiativeRow>> {
    f.store
        .list_initiatives(project)?
        .iter()
        .map(|i| initiative_row(f, i))
        .collect()
}

/// One lineage in `InitiativeDoc.tasks`: its latest task's id, state and
/// reason, plus how many retries the lineage took to reach it.
#[derive(Serialize)]
pub struct InitiativeTaskRow {
    pub id: i64,
    pub state: String,
    pub reason: String,
    pub retries: i64,
    /// The assess directive's maintainability score for this task's own
    /// landing, 0-10; `None` when it never ran (workflow does not opt
    /// in, the task never landed, or the run failed; see
    /// docs/ACTIONS.md, "Assessment").
    pub score: Option<i64>,
    /// Total cost across every attempt of this lineage's latest task
    /// (`Store::task_cost`), so the report's task table can show what
    /// each one spent alongside the initiative's own total.
    pub cost_usd: f64,
}

/// One row of `InitiativeDoc.refused`: a verification rule name and how
/// many attempts of the initiative's tasks it refused.
#[derive(Serialize)]
pub struct RefusedRow {
    pub rule: String,
    pub count: i64,
}

/// One row of `InitiativeDoc.rulings`: a decision the supervisor made on
/// one of the initiative's tasks.
#[derive(Serialize)]
pub struct InitiativeRulingRow {
    pub task_id: i64,
    pub question: String,
    pub answer: String,
    pub citations: String,
}

/// One row of `InitiativeDoc.questions`: a question that reached the
/// operator, answered or (while the task is still blocked) not yet.
#[derive(Serialize)]
pub struct InitiativeQuestionRow {
    pub task_id: i64,
    pub question: String,
    pub answer: Option<String>,
}

/// One row of `InitiativeDoc.deployed`: a deploy one of the initiative's
/// tasks triggered on landing (see docs/DEPLOY.md, "When a deploy runs").
/// `findings` is `deploy-look`'s verdict, parsed from the deploy row's
/// `look_json`, empty when it never ran.
#[derive(Serialize)]
pub struct InitiativeDeployRow {
    pub task_id: i64,
    pub target: String,
    pub sha: String,
    pub check_ok: Option<bool>,
    pub rolled_back_to: Option<String>,
    pub findings: Vec<crate::deploy_look::Finding>,
}

/// How many attempts of `tasks` each verification rule refused, by name,
/// ordered by name.
fn refused_counts(f: &Forge, tasks: &[Task]) -> Result<Vec<RefusedRow>> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for t in tasks {
        for a in f.store.attempts(t.id)? {
            let rows: Vec<crate::checks::CheckResult> =
                serde_json::from_str(&a.verdict_json).unwrap_or_default();
            for c in rows.iter().filter(|c| !c.ok) {
                *counts.entry(c.name.clone()).or_default() += 1;
            }
        }
    }
    Ok(counts
        .into_iter()
        .map(|(rule, count)| RefusedRow { rule, count })
        .collect())
}

/// The generated report `forge initiative report` shows: the outcome,
/// each task and how it ended, what verification refused, what the
/// supervisor ruled, what reached the operator, cost and elapsed time
/// (see docs/PROJECTS.md, "One notification and one report").
#[derive(Serialize)]
pub struct InitiativeDoc {
    pub id: i64,
    pub project: String,
    pub outcome: String,
    pub state: String,
    pub held_rule: Option<String>,
    pub budget_usd: Option<f64>,
    pub stop_after_same_rule: i64,
    pub tasks: Vec<InitiativeTaskRow>,
    pub refused: Vec<RefusedRow>,
    pub rulings: Vec<InitiativeRulingRow>,
    pub questions: Vec<InitiativeQuestionRow>,
    pub deployed: Vec<InitiativeDeployRow>,
    pub cost_usd: f64,
    pub elapsed_secs: Option<i64>,
    pub created_at: i64,
    pub settled_at: Option<i64>,
    /// The escalator's proposal this initiative came from, when a "yes"
    /// answer filed it (see docs/INTAKE.md, "The escalator"); `None` for
    /// an initiative filed any other way.
    pub proposal: Option<ProposalRow>,
}

pub fn initiative_doc(f: &Forge, ini: &crate::store::Initiative) -> Result<InitiativeDoc> {
    let tasks = f.store.initiative_tasks(ini.id)?;
    let lineages = latest_per_lineage(f, &tasks)?;
    let latest: Vec<Task> = lineages.iter().map(|(t, _)| t.clone()).collect();
    let hold = initiative_hold(f, ini)?;
    let state = initiative_state(&latest, hold.as_deref()).to_string();
    let cost = f.store.initiative_cost(ini.id)?;
    let elapsed = tasks
        .iter()
        .filter_map(|t| t.finished_at)
        .max()
        .map(|end| end - ini.created_at);
    let decisions = f.store.decisions(&crate::store::DecisionFilter {
        initiative: Some(ini.id),
        ..Default::default()
    })?;
    // Scoped by initiative above, so a task-less admin decision (`forge
    // stats --reprice`, `task_id: None`) never reaches here; filter_map
    // just keeps that guarantee honest rather than unwrapping blindly.
    let rulings = decisions
        .iter()
        .filter(|d| d.answered_by == "supervisor")
        .filter_map(|d| {
            Some(InitiativeRulingRow {
                task_id: d.task_id?,
                question: d.question.clone(),
                answer: d.answer.clone(),
                citations: d.citations.clone(),
            })
        })
        .collect();
    let mut questions: Vec<InitiativeQuestionRow> = decisions
        .iter()
        .filter(|d| d.answered_by == "operator")
        .filter_map(|d| {
            Some(InitiativeQuestionRow {
                task_id: d.task_id?,
                question: d.question.clone(),
                answer: Some(d.answer.clone()),
            })
        })
        .collect();
    for t in tasks.iter().filter(|t| t.state == TaskState::Blocked) {
        if !questions.iter().any(|q| q.task_id == t.id) {
            let (_, text) = request_kind(&t.reason);
            questions.push(InitiativeQuestionRow {
                task_id: t.id,
                question: text,
                answer: None,
            });
        }
    }
    let proposal = f
        .store
        .project_tasks(&ini.project)?
        .iter()
        .find(|t| t.proposal_initiative == Some(ini.id))
        .and_then(proposal_row);
    let refused = refused_counts(f, &tasks)?;
    let mut deployed = Vec::new();
    for t in &tasks {
        for d in f.store.deploys_for_task(t.id)? {
            let findings = d
                .look_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok())
                .unwrap_or_default();
            deployed.push(InitiativeDeployRow {
                task_id: t.id,
                target: d.target,
                sha: d.sha,
                check_ok: d.check_ok,
                rolled_back_to: d.rolled_back_to,
                findings,
            });
        }
    }
    Ok(InitiativeDoc {
        id: ini.id,
        project: ini.project.clone(),
        outcome: ini.outcome.clone(),
        state,
        held_rule: hold,
        budget_usd: ini.budget_usd,
        stop_after_same_rule: ini.stop_after_same_rule,
        tasks: lineages
            .iter()
            .map(|(t, retries)| {
                Ok(InitiativeTaskRow {
                    id: t.id,
                    state: t.state.as_str().to_string(),
                    reason: t.reason.clone(),
                    retries: *retries,
                    score: f.store.assessment(t.id)?.map(|a| a.score),
                    cost_usd: f.store.task_cost(t.id)?,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        refused,
        rulings,
        questions,
        deployed,
        cost_usd: cost,
        elapsed_secs: elapsed,
        created_at: ini.created_at,
        settled_at: ini.settled_at,
        proposal,
    })
}

/// The escalator's pattern (docs/INTAKE.md, "The escalator"), as
/// `forge ask` records it on a proposal placeholder's `proposal_json`:
/// the quoted requests that share a shape, why, and the outcome an
/// initiative would pursue if the operator says yes. Written by
/// `concierge::ask`, read back here to build `ProposalRow`.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProposalRecord {
    pub task_ids: Vec<i64>,
    pub repetition: String,
    pub outcome: String,
}

/// One row of a project's or an initiative's proposals: the escalator's
/// question, who it went to, and how it was answered (see
/// docs/INTAKE.md, "The escalator"). `answer` is `None` while the
/// placeholder task is still blocked; `initiative` is set only by a
/// "yes".
#[derive(Serialize)]
pub struct ProposalRow {
    pub task_id: i64,
    pub quoted: Vec<i64>,
    pub repetition: String,
    pub outcome: String,
    pub to: Option<String>,
    pub answer: Option<String>,
    pub initiative: Option<i64>,
}

/// Build a `ProposalRow` from a task the escalator blocked, or `None` for
/// any other task (`proposal_json` unset or unreadable).
pub fn proposal_row(t: &Task) -> Option<ProposalRow> {
    let raw = t.proposal_json.as_deref()?;
    let p: ProposalRecord = serde_json::from_str(raw).ok()?;
    Some(ProposalRow {
        task_id: t.id,
        quoted: p.task_ids,
        repetition: p.repetition,
        outcome: p.outcome,
        to: t.question_to.clone(),
        answer: t.proposal_answer.clone(),
        initiative: t.proposal_initiative,
    })
}

/// The brief an `intake` task's `interview` directive writes to `t.plan`
/// once its checklist is satisfied (see docs/INTAKE.md, "Mechanics").
#[derive(Debug, Deserialize)]
pub(crate) struct Brief {
    pub(crate) workflows: Vec<BriefWorkflow>,
    pub(crate) where_it_runs: String,
    #[serde(default)]
    pub(crate) confirmed: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BriefWorkflow {
    pub(crate) name: String,
    trigger: String,
    inputs: String,
    outputs: String,
    other_people: String,
    failure_today: String,
    success_signal: String,
    do_not_touch: String,
}

/// One workflow's fields, in the person's own words, as a paragraph: the
/// backlog entry `intake accept` files for it, the project's purpose (for
/// the first workflow named), and one entry of `PortalDoc.brief`.
pub(crate) fn workflow_paragraph(w: &BriefWorkflow) -> String {
    format!(
        "{}: starts when {}. Takes in {} and produces {}. Involves {}. Today, {}. Working would look like: {}. Must not change: {}.",
        w.name,
        w.trigger,
        w.inputs,
        w.outputs,
        w.other_people,
        w.failure_today,
        w.success_signal,
        w.do_not_touch,
    )
}

/// One deploy target on `PortalDoc`: the customer's "Running for you"
/// list (see docs/PORTAL.md, "What they see"). No method, args, check
/// command or repo path — those are how, not what.
#[derive(Serialize)]
pub struct PortalDeployTarget {
    pub name: String,
    pub where_it_runs: String,
    pub last_deployed_at: Option<i64>,
    pub check_ok: Option<bool>,
    pub look_ok: Option<bool>,
    pub screenshot: Option<String>,
}

/// One of a run workflow's last three jobs on `PortalDoc`, "Running for
/// you" continued (see docs/PORTAL.md): when it ran, whether it went
/// `"ok"`, `"failed"` or `"needs_human"` (`store::JobState::as_str()`),
/// whether it was only rehearsed (`store::Job::dry_run`), and every
/// effect it logged, each cut to its own one-line `summary` — never its
/// `kind` or `target` (see `store::JobEffect`). `reason`, on a failure or
/// a needs-human run, is a one-line cause cut from the first failing
/// check's tail: the human rung's question, in the customer's own words.
/// `None` on a needs-human run whose question did not actually go to this
/// project's own contact — its resolved workflow's `[limits] on_failure`
/// is anything but `"ask:contact"`, or is `"ask:contact"` but
/// `job::trigger_contact` found no one to ask (see `job_trigger_contact`)
/// — means the operator got the question instead, and the portal shows
/// "we're on it" rather than a reason the customer was never asked to
/// answer. No job id, no cost, no trigger, no verdict rows — the
/// operator's `forge job show` carries those.
#[derive(Serialize)]
pub struct PortalJobRun {
    pub started_at: i64,
    pub state: String,
    pub dry_run: bool,
    pub effects: Vec<String>,
    pub reason: Option<String>,
}

/// One run workflow on `PortalDoc`: an automation this project's jobs
/// run through, its own workflow file's `description` (empty when the
/// file can no longer be resolved), and its last three jobs, newest
/// first, from the same job rows `forge job list` serves (see
/// docs/PORTAL.md, "What they see").
#[derive(Serialize)]
pub struct PortalWorkflow {
    pub name: String,
    pub description: String,
    pub jobs: Vec<PortalJobRun>,
}

/// One open initiative on `PortalDoc`: the customer's "Being built" list,
/// newest first, capped at ten (`PortalDoc.initiatives_more` the rest —
/// see docs/PORTAL.md). `state` is always one of "in progress" or
/// "waiting on you" — never the operator's `open`/`held` vocabulary.
/// `pieces` is how many tasks make up the initiative so far.
#[derive(Serialize)]
pub struct PortalInitiative {
    pub outcome: String,
    pub state: String,
    pub pieces: i64,
    /// How many of those tasks have landed: "n of m" on the page.
    pub done: i64,
}

/// One open question on `PortalDoc`, addressed to the customer: the
/// "Needs you" list. `task_id` is what answering in place posts back
/// against (`forge answer <task_id> ...`); `asked_at` is Unix seconds, when
/// the task blocked on it.
#[derive(Serialize)]
pub struct PortalQuestion {
    pub task_id: i64,
    pub text: String,
    pub asked_at: i64,
}

/// One line on `PortalDoc`'s "Done" list, newest first, capped at ten
/// (`PortalDoc.landed_more` the rest — see docs/PORTAL.md): a landed
/// initiative's outcome sentence (`pieces` how many tasks it took), or a
/// landed task that belongs to no initiative (`pieces` `None`), `text`
/// its title if it has one, else a line derived from its request — never
/// the operator's full text, and never a path-like token.
#[derive(Serialize)]
pub struct PortalLanded {
    pub text: String,
    pub pieces: Option<i64>,
    pub landed_at: i64,
    /// When a deploy that followed this landing went live (Unix seconds);
    /// `None` when none did.
    pub deployed_at: Option<i64>,
    /// That deploy's own id, what the portal routes its look-step
    /// screenshot by; `Some` only when the screenshot exists.
    pub deploy_id: Option<i64>,
    /// The look step's screenshot file, for the portal's own streaming;
    /// never rendered.
    pub screenshot: Option<String>,
}

/// One line on `PortalDoc`'s "Your requests" list, newest first, capped at
/// ten: a task the customer asked for that belongs to no initiative.
/// `state` is always one of "waiting", "being built", "needs you" or
/// "done" — never the operator's task vocabulary. `text` is its title if
/// it has one, else a line derived from the request.
#[derive(Serialize)]
pub struct PortalRequest {
    pub text: String,
    pub state: String,
    pub created_at: i64,
}

/// The confirmed intake brief on `PortalDoc`, in the person's own words
/// (see docs/INTAKE.md): "Your plan".
#[derive(Serialize)]
pub struct PortalBrief {
    pub where_it_runs: String,
    pub workflows: Vec<String>,
}

/// One open backlog item on `PortalDoc`: the rest of "Your plan", what
/// is queued but not yet running.
#[derive(Serialize)]
pub struct PortalBacklogItem {
    pub id: i64,
    pub text: String,
    pub created_at: i64,
}

/// The document `forge project view NAME --json` prints: everything the
/// customer portal's page needs for one project, in their own words (see
/// docs/PORTAL.md, "What they see"). No ids beyond a task's own (needed
/// to answer a question in place), no branches, no costs, no attempt
/// data, no verdict rows — the operator's page shows those; this shows
/// only what the customer asked for and what they're waiting on.
#[derive(Serialize)]
pub struct PortalDoc {
    pub project: String,
    /// The project's purpose, for the operator's own tools; never
    /// rendered on the customer's page (see docs/PORTAL.md).
    pub purpose: String,
    pub deploy_targets: Vec<PortalDeployTarget>,
    pub run_workflows: Vec<PortalWorkflow>,
    pub initiatives: Vec<PortalInitiative>,
    /// How many open initiatives past the ten in `initiatives` — "and n
    /// more" (0 when nothing was cut).
    pub initiatives_more: i64,
    pub questions: Vec<PortalQuestion>,
    pub landed: Vec<PortalLanded>,
    /// How many landed lines past the ten in `landed` — "and n more" (0
    /// when nothing was cut).
    pub landed_more: i64,
    /// The customer's own requests and where each stands, newest first,
    /// capped at ten.
    pub requests: Vec<PortalRequest>,
    pub brief: Option<PortalBrief>,
    pub backlog: Vec<PortalBacklogItem>,
}

pub fn portal_doc(f: &Forge, p: &crate::store::Project) -> Result<PortalDoc> {
    let mut deploy_targets = Vec::new();
    for t in f.store.deploy_targets(&p.name)? {
        let last = f.store.deploys(&p.name, Some(&t.name))?.into_iter().next();
        let (last_deployed_at, check_ok, look_ok, screenshot) = match &last {
            Some(d) => (
                Some(d.started_at),
                d.check_ok,
                d.look_ok,
                d.smoke_json.as_ref().map(|_| {
                    f.paths
                        .home
                        .join("deploys")
                        .join(d.id.to_string())
                        .join("screenshot.png")
                        .display()
                        .to_string()
                }),
            ),
            None => (None, None, None, None),
        };
        deploy_targets.push(PortalDeployTarget {
            name: t.name,
            where_it_runs: t.args.get("host").cloned().unwrap_or_default(),
            last_deployed_at,
            check_ok,
            look_ok,
            screenshot,
        });
    }

    // Running for you, continued: every run workflow this project's jobs
    // have used, newest job first, each capped at its last three (see
    // docs/PORTAL.md). `f.store.jobs` already orders newest first, so a
    // single pass building each workflow's entry in first-seen order
    // keeps both the workflow order and each one's job order correct.
    // Each workflow's file is resolved once, the same repository-then-
    // catalog lookup `forge job start` uses, for its `description` and
    // its `[limits] on_failure` policy (whether a needs-human run's
    // question was addressed to this project's contact or to the
    // operator).
    let repo = f.store.first_repo(&p.name)?;
    let mut resolved: std::collections::HashMap<String, Option<crate::workflows::Workflow>> =
        Default::default();
    let mut run_workflows: Vec<PortalWorkflow> = Vec::new();
    for j in f.store.jobs(Some(&p.name), None)? {
        let wf = resolved
            .entry(j.workflow.clone())
            .or_insert_with(|| run_workflow(f, repo.as_deref(), &j));
        let entry = match run_workflows.iter().position(|w| w.name == j.workflow) {
            Some(i) => &mut run_workflows[i],
            None => {
                run_workflows.push(PortalWorkflow {
                    name: j.workflow.clone(),
                    description: wf
                        .as_ref()
                        .map(|w| w.description.clone())
                        .unwrap_or_default(),
                    jobs: Vec::new(),
                });
                run_workflows.last_mut().expect("just pushed")
            }
        };
        if entry.jobs.len() < 3 {
            // A needs-human run carries a customer-facing reason only when
            // the question actually went to this project's own contact:
            // its resolved workflow's `[limits] on_failure = "ask:contact"`
            // *and* `job::trigger_contact` finds someone to ask (the same
            // two facts `job::run_now` used to decide who to ask). Every
            // other needs-human run — `ask:operator`, an `ask:contact` run
            // with no contact to ask, an unresolved workflow, or a
            // needs-human run with no `[limits]` at all (a budget overrun,
            // say) — is the operator's, and carries no reason at all; the
            // portal shows "we're on it" for it instead (see
            // `PortalJobRun`).
            let to_operator = j.state == crate::store::JobState::NeedsHuman
                && !(matches!(
                    wf.as_ref()
                        .and_then(|w| w.limits.as_ref())
                        .map(|l| &l.on_failure),
                    Some(crate::workflows::OnFailure::AskContact)
                ) && job_trigger_contact(f, &j, wf.as_ref()).is_some());
            let reason = if to_operator { None } else { job_reason(&j) };
            let effects = f.store.job_effects(j.id).unwrap_or_default();
            entry.jobs.push(PortalJobRun {
                started_at: j.started_at,
                state: j.state.as_str().to_string(),
                dry_run: j.dry_run,
                effects: effects.into_iter().map(|e| e.summary).collect(),
                reason,
            });
        }
    }

    let tasks = f.store.project_tasks(&p.name)?;
    let latest: Vec<Task> = latest_per_lineage(f, &tasks)?
        .into_iter()
        .map(|(t, _)| t)
        .collect();

    // A blocked task whose kind is "question" is what the customer sees
    // under Needs you; it also flips its own initiative's plain state to
    // "waiting on you" below, the two lists staying consistent with each
    // other by construction.
    let mut questions = Vec::new();
    let mut questioning_initiatives: std::collections::BTreeSet<i64> = Default::default();
    for t in latest.iter().filter(|t| t.state == TaskState::Blocked) {
        let (kind, text) = request_kind(&t.reason);
        if kind == "question" {
            questions.push(PortalQuestion {
                task_id: t.id,
                text,
                asked_at: t.finished_at.unwrap_or(t.created_at),
            });
            if let Some(ini) = t.initiative {
                questioning_initiatives.insert(ini);
            }
        }
    }

    let all_initiatives = initiative_rows(f, Some(&p.name))?;

    // Being built: every open initiative (never settled), newest first,
    // capped at ten.
    let mut open: Vec<&InitiativeRow> = all_initiatives
        .iter()
        .filter(|r| r.settled_at.is_none())
        .collect();
    open.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let initiatives_total = open.len();
    let initiatives: Vec<PortalInitiative> = open
        .into_iter()
        .take(10)
        .map(|r| {
            let state = if questioning_initiatives.contains(&r.id) {
                "waiting on you"
            } else {
                "in progress"
            };
            PortalInitiative {
                outcome: r.outcome.clone(),
                state: state.to_string(),
                pieces: initiative_pieces(r),
                done: r.succeeded,
            }
        })
        .collect();
    let initiatives_more = (initiatives_total - initiatives.len()) as i64;

    // Done: a landed initiative is one line, its outcome and how many
    // tasks it took; a landed task belonging to no initiative is one
    // line, its title or a line derived from its request. Merged, newest
    // first, capped at ten.
    let mut landed: Vec<PortalLanded> = Vec::new();
    let all_deploys = f.store.deploys(&p.name, None)?;
    // The newest deploy that went live for any of `task_ids`.
    let deploy_for = |task_ids: &[i64]| {
        all_deploys
            .iter()
            .filter(|d| {
                d.task_id.is_some_and(|t| task_ids.contains(&t))
                    && d.check_ok == Some(true)
                    && d.rolled_back_to.is_none()
            })
            .max_by_key(|d| d.started_at)
            .map(|d| {
                let shot = d.smoke_json.as_ref().map(|_| {
                    f.paths
                        .home
                        .join("deploys")
                        .join(d.id.to_string())
                        .join("screenshot.png")
                        .display()
                        .to_string()
                });
                (d.started_at, shot.map(|s| (d.id, s)))
            })
    };
    for r in all_initiatives.iter().filter(|r| r.settled_at.is_some()) {
        let ids: Vec<i64> = f
            .store
            .initiative_tasks(r.id)?
            .iter()
            .map(|t| t.id)
            .collect();
        let dep = deploy_for(&ids);
        landed.push(PortalLanded {
            text: r.outcome.clone(),
            pieces: Some(initiative_pieces(r)),
            landed_at: r.settled_at.unwrap_or(0),
            deployed_at: dep.as_ref().map(|d| d.0),
            deploy_id: dep.as_ref().and_then(|d| d.1.as_ref().map(|s| s.0)),
            screenshot: dep.and_then(|d| d.1.map(|s| s.1)),
        });
    }
    for t in latest
        .iter()
        .filter(|t| !t.landed_sha.is_empty() && t.initiative.is_none())
    {
        let dep = deploy_for(&[t.id]);
        landed.push(PortalLanded {
            text: crate::render::landed_task_line(t),
            pieces: None,
            landed_at: t.finished_at.unwrap_or(0),
            deployed_at: dep.as_ref().map(|d| d.0),
            deploy_id: dep.as_ref().and_then(|d| d.1.as_ref().map(|s| s.0)),
            screenshot: dep.and_then(|d| d.1.map(|s| s.1)),
        });
    }
    landed.sort_by(|a, b| b.landed_at.cmp(&a.landed_at));
    let landed_total = landed.len();
    landed.truncate(10);
    let landed_more = (landed_total - landed.len()) as i64;

    // Your requests: each task outside an initiative, in plain words.
    let mut requests: Vec<PortalRequest> = latest
        .iter()
        .filter(|t| t.initiative.is_none() && t.state != TaskState::Withdrawn)
        .map(|t| {
            let state = match t.state {
                _ if !t.landed_sha.is_empty() => "done",
                TaskState::Blocked if request_kind(&t.reason).0 == "question" => "needs you",
                TaskState::Running | TaskState::Succeeded => "being built",
                _ => "waiting",
            };
            PortalRequest {
                text: crate::render::landed_task_line(t),
                state: state.to_string(),
                created_at: t.created_at,
            }
        })
        .collect();
    requests.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    requests.truncate(10);

    // The confirmed brief lives only on the intake task that produced it
    // (`t.plan`, see docs/INTAKE.md); a project carries no copy of its
    // own, so the most recent confirmed one is re-read here.
    let brief = tasks
        .iter()
        .filter(|t| t.workflow == "intake" && !t.plan.is_empty())
        .filter_map(|t| serde_json::from_str::<Brief>(&t.plan).ok())
        .rfind(|b| b.confirmed)
        .map(|b| PortalBrief {
            where_it_runs: b.where_it_runs,
            workflows: b.workflows.iter().map(workflow_paragraph).collect(),
        });

    let backlog = f
        .store
        .backlog(&p.name)?
        .into_iter()
        .filter(|b| b.done_at.is_none())
        .map(|b| PortalBacklogItem {
            id: b.id,
            text: b.text,
            created_at: b.created_at,
        })
        .collect();

    Ok(PortalDoc {
        project: p.name.clone(),
        purpose: real_purpose(&p.purpose),
        deploy_targets,
        run_workflows,
        initiatives,
        initiatives_more,
        questions,
        landed,
        landed_more,
        requests,
        brief,
        backlog,
    })
}

/// The workflow one job ran, resolved the same way `forge job start`
/// resolves it (docs/JOBS.md, "Where an automation lives"): the
/// project's own repository at the job's `landed_sha` when the job came
/// from there, else the operator's catalog by name. `None` when neither
/// has a workflow of that name any more — an edited-away or removed file
/// leaves "Running for you" with no description and no `on_failure`
/// policy to gate a needs-human reason on, rather than failing the read.
fn run_workflow(
    f: &Forge,
    repo: Option<&str>,
    j: &crate::store::Job,
) -> Option<crate::workflows::Workflow> {
    if j.workflow_source == crate::workflows::JobSource::Repo.as_str()
        && !j.landed_sha.is_empty()
        && let Some(repo) = repo
        && let Ok(all) = crate::workflows::load_all_at(std::path::Path::new(repo), &j.landed_sha)
        && let Some(w) = all.into_iter().find(|w| w.name == j.workflow)
    {
        return Some(w);
    }
    crate::workflows::get(&f.paths.home, &j.workflow)
        .ok()
        .flatten()
}

/// Who a needs-human run's question actually went to, the same fact
/// `job::run_now` decided it with: the job's saved input (`input.json`
/// under its input directory, the same file `job::run_now` wrote before
/// the run and later re-reads for a retry) and the resolved workflow's
/// own `[trigger]`, fed to `job::trigger_contact`. `None` when the
/// workflow could not be resolved, carries no trigger, or the contact
/// can't be determined — the caller then knows the question went to the
/// operator instead.
fn job_trigger_contact(
    f: &Forge,
    j: &crate::store::Job,
    wf: Option<&crate::workflows::Workflow>,
) -> Option<String> {
    let idir = crate::job::input_dir(f, j.id);
    let input_text =
        std::fs::read_to_string(idir.join("input.json")).unwrap_or_else(|_| "{}".into());
    let input_json: serde_json::Value =
        serde_json::from_str(&input_text).unwrap_or(serde_json::Value::Null);
    crate::job::trigger_contact(j, wf.and_then(|w| w.trigger.as_ref()), &input_json)
}

/// A failed, needs-human, or skipped job's one-line reason on `PortalDoc`:
/// for a failure, the first line of the first failing check's tail in
/// `verdict_json`; for `Skipped`, the first line of the `[skip_if]`
/// command's own tail — its stdout's first line, recorded there by
/// `job::run_now` (docs/JOBS.md, "Skipping a run"). Either way, any
/// path-like token is stripped and the result cut at 120 characters on a
/// word boundary, the same treatment `derive_landed_line` gives a landed
/// task's own request text (see docs/PORTAL.md). `None` for a job that is
/// queued, running, dropped, or went ok, or whose verdict carries no
/// matching check.
fn job_reason(j: &crate::store::Job) -> Option<String> {
    use crate::store::JobState;
    let verdict: Vec<crate::checks::CheckResult> =
        serde_json::from_str(&j.verdict_json).unwrap_or_default();
    let tail = match j.state {
        JobState::Skipped => &verdict.first()?.tail,
        JobState::Failed | JobState::NeedsHuman => &verdict.iter().find(|c| !c.ok)?.tail,
        _ => return None,
    };
    let line = tail.lines().next().unwrap_or(tail);
    let stripped = crate::render::strip_path_like_tokens(line.trim());
    Some(crate::render::truncate_at_word_boundary(
        stripped.trim(),
        120,
    ))
}

/// How many tasks make up an initiative so far — every lineage, whatever
/// state it's in — the "n pieces of work" beside its outcome on
/// `PortalDoc` (see docs/PORTAL.md).
fn initiative_pieces(r: &InitiativeRow) -> i64 {
    r.queued + r.running + r.succeeded + r.failed + r.unverified + r.blocked + r.withdrawn
}

#[cfg(test)]
#[path = "lineage_rollup_tests.rs"]
mod lineage_rollup_tests;

#[cfg(test)]
#[path = "stop_rule_tests.rs"]
mod stop_rule_tests;

#[cfg(test)]
#[path = "portal_tests.rs"]
mod portal_tests;
