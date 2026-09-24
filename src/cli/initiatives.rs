use super::*;
use crate::workflows;

#[derive(Subcommand)]
pub(super) enum InitiativeCmd {
    /// Register a new initiative and, with --from, file its tasks
    New {
        project: String,
        /// One sentence saying what is true when the initiative is done;
        /// placed in every task's prompt as "Why this task exists"
        #[arg(long)]
        outcome: String,
        /// A file of task texts, one per paragraph (blank-line
        /// separated); a paragraph may lead with `after: <n>` (an
        /// earlier paragraph's 1-based number, as a dependency),
        /// `repo: <path>` (else the project's first repository),
        /// `provider: <name>` (else --provider's default) and
        /// `workflow: <name>` (else --workflow's default)
        #[arg(long)]
        from: Option<PathBuf>,
        /// The provider every paragraph runs under unless it names its
        /// own `provider:` (default: "anthropic"); validated against
        /// `[providers.<name>]` (see `forge providers`) when the file is
        /// read
        #[arg(long)]
        provider: Option<String>,
        /// The workflow every paragraph runs under unless it names its
        /// own `workflow:` (default: the project's, else "direct");
        /// validated against the workflow catalog (see `forge
        /// workflows`) when the file is read
        #[arg(long)]
        workflow: Option<String>,
        /// This initiative's own cost cap in USD (default: the
        /// project's per-initiative-usd)
        #[arg(long)]
        budget: Option<f64>,
        /// Hold the initiative after this many of its tasks fail in a
        /// row on the same L0 rule (default: 3)
        #[arg(long = "stop-after")]
        stop_after: Option<u32>,
    },
    /// File a task's recorded plan (from the investigate directive) into
    /// a new initiative: one task per plan item, chained in order,
    /// against the task's repository, in the task's project
    FromPlan {
        /// The task whose `t.plan` is filed; refused if it has none
        task: i64,
        /// The initiative's outcome (default: the task's own text)
        #[arg(long)]
        outcome: Option<String>,
    },
    /// Change an existing initiative, replacing only the fields given;
    /// refused when none are, and prints the initiative afterward
    Set {
        id: i64,
        /// This initiative's own cost cap in USD
        #[arg(long)]
        budget: Option<f64>,
        /// Hold the initiative after this many of its tasks fail in a
        /// row on the same L0 rule
        #[arg(long = "stop-after")]
        stop_after: Option<u32>,
        /// One sentence saying what is true when the initiative is done
        #[arg(long)]
        outcome: Option<String>,
    },
    /// Every initiative, its state, task counts and cost
    List {
        /// Only this project's
        project: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// One initiative: its state, task counts, cost and settings
    Show {
        id: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// The generated report: outcome, each task's fate, what
    /// verification refused, supervisor rulings, questions that reached
    /// the operator, cost and elapsed time
    Report {
        id: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

fn print_initiative_row(r: &crate::view::InitiativeRow) {
    out!("id         {}", r.id);
    out!("project    {}", r.project);
    out!("outcome    {}", r.outcome);
    out!(
        "state      {}{}",
        r.state,
        r.held_rule
            .as_deref()
            .map(|rule| format!(" ({rule})"))
            .unwrap_or_default()
    );
    out!(
        "tasks      queued={} running={} succeeded={} failed={} unverified={} blocked={} withdrawn={}",
        r.queued,
        r.running,
        r.succeeded,
        r.failed,
        r.unverified,
        r.blocked,
        r.withdrawn
    );
    out!(
        "cost       ${:.2}{}",
        r.cost_usd,
        r.budget_usd
            .map(|b| format!(" of ${b:.2}"))
            .unwrap_or_default()
    );
    out!("stop-after {}", r.stop_after_same_rule);
    out!("created_at {}", render::utc(r.created_at));
    if let Some(at) = r.settled_at {
        out!("settled_at {}", render::utc(at));
    }
}

pub(super) async fn initiative_new(
    project: String,
    outcome: String,
    from: Option<PathBuf>,
    provider: Option<String>,
    workflow: Option<String>,
    budget: Option<f64>,
    stop_after: Option<u32>,
) -> Result<()> {
    if let Some(b) = budget
        && b <= 0.0
    {
        bail!("budget must be positive");
    }
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let id = f.store.create_initiative(&crate::store::Initiative {
        project: project.clone(),
        outcome,
        budget_usd: budget,
        stop_after_same_rule: stop_after.map(|n| n as i64).unwrap_or(3),
        created_at: unix_now(),
        ..Default::default()
    })?;
    out!("created initiative {id}");
    if let Some(path) = from {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let default_repo = f.store.first_repo(&project)?;
        let paragraphs = crate::queue::parse_initiative_file(&text)?;
        if let Some(p) = &provider {
            f.providers.get(p).with_context(|| {
                format!("unknown provider {p:?}; see `forge providers` for what is configured")
            })?;
        }
        if let Some(w) = &workflow {
            workflows::get(&f.paths.home, w)?.with_context(|| {
                format!("unknown workflow {w:?}; see `forge workflows` for what is configured")
            })?;
        }
        crate::queue::validate_initiative_file(&f, &paragraphs)?;
        let ids = crate::queue::file_initiative_paragraphs(
            &f,
            &project,
            id,
            &paragraphs,
            default_repo.as_deref(),
            provider.as_deref(),
            workflow.as_deref(),
        )
        .await?;
        for (n, tid) in ids.iter().enumerate() {
            out!("queued task {tid} (paragraph {})", n + 1);
        }
        out!("filed {} of {} tasks", ids.len(), paragraphs.len());
    }
    Ok(())
}

pub(super) async fn initiative_from_plan(task: i64, outcome: Option<String>) -> Result<()> {
    let f = Forge::open(false, false)?;
    let t = f
        .store
        .task(task)?
        .with_context(|| format!("no task {task}"))?;
    if t.plan.is_empty() {
        bail!(
            "task {task} has no recorded plan (the investigate directive did not run, or found none)"
        );
    }
    let project = t
        .project
        .clone()
        .with_context(|| format!("task {task} has no project"))?;
    let id = f.store.create_initiative(&crate::store::Initiative {
        project,
        outcome: outcome.unwrap_or_else(|| t.task.clone()),
        stop_after_same_rule: 3,
        created_at: unix_now(),
        ..Default::default()
    })?;
    out!("created initiative {id}");
    let ids = crate::queue::file_plan(&f, &t, id).await?;
    for (n, tid) in ids.iter().enumerate() {
        out!("queued task {tid} (plan item {})", n + 1);
    }
    Ok(())
}

pub(super) fn initiative_set(
    id: i64,
    budget: Option<f64>,
    stop_after: Option<u32>,
    outcome: Option<String>,
) -> Result<()> {
    if budget.is_none() && stop_after.is_none() && outcome.is_none() {
        bail!("nothing to set: pass --budget, --stop-after or --outcome");
    }
    if let Some(b) = budget
        && b <= 0.0
    {
        bail!("budget must be positive");
    }
    let f = Forge::open(false, false)?;
    if !f.store.set_initiative(
        id,
        &crate::store::InitiativeUpdate {
            outcome,
            budget_usd: budget,
            stop_after_same_rule: stop_after.map(|n| n as i64),
        },
    )? {
        bail!("no initiative {id}");
    }
    let ini = f.store.initiative(id)?.context("initiative vanished")?;
    let row = crate::view::initiative_row(&f, &ini)?;
    print_initiative_row(&row);
    Ok(())
}

pub(super) fn initiative_list(project: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let rows = crate::view::initiative_rows(&f, project.as_deref())?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no initiatives");
        return Ok(());
    }
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out!();
        }
        print_initiative_row(r);
    }
    Ok(())
}

pub(super) fn initiative_show(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let ini = f
        .store
        .initiative(id)?
        .with_context(|| format!("no initiative {id}"))?;
    let row = crate::view::initiative_row(&f, &ini)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&row)?);
        return Ok(());
    }
    print_initiative_row(&row);
    Ok(())
}

pub(super) fn initiative_report(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let ini = f
        .store
        .initiative(id)?
        .with_context(|| format!("no initiative {id}"))?;
    let doc = crate::view::initiative_doc(&f, &ini)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    out!("initiative {} ({})", doc.id, doc.project);
    out!("outcome    {}", doc.outcome);
    if let Some(p) = &doc.proposal {
        out!("proposal   task {} — {}", p.task_id, p.repetition);
    }
    out!(
        "state      {}{}",
        doc.state,
        doc.held_rule
            .as_deref()
            .map(|rule| format!(" ({rule})"))
            .unwrap_or_default()
    );
    out!(
        "cost       ${:.2}{}",
        doc.cost_usd,
        doc.budget_usd
            .map(|b| format!(" of ${b:.2}"))
            .unwrap_or_default()
    );
    out!(
        "elapsed    {}",
        doc.elapsed_secs
            .map_or("-".to_string(), |s| format!("{s}s"))
    );
    out!("tasks");
    for t in &doc.tasks {
        out!(
            "  {:<5} {:<10}${:<7.2}{}{}{}",
            t.id,
            t.state,
            t.cost_usd,
            match t.retries {
                0 => String::new(),
                1 => " (1 retry)".to_string(),
                n => format!(" ({n} retries)"),
            },
            t.score
                .map(|s| format!(" score {s}/10"))
                .unwrap_or_default(),
            if t.reason.is_empty() {
                String::new()
            } else {
                format!(" {}", t.reason)
            }
        );
    }
    if doc.refused.is_empty() {
        out!("refused    none");
    } else {
        out!(
            "refused    {}",
            doc.refused
                .iter()
                .map(|r| format!("{} x{}", r.rule, r.count))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if doc.rulings.is_empty() {
        out!("rulings    none");
    } else {
        out!("rulings");
        for r in &doc.rulings {
            out!("  task {} Q: {} A: {}", r.task_id, r.question, r.answer);
        }
    }
    if doc.questions.is_empty() {
        out!("questions  none");
    } else {
        out!("questions");
        for q in &doc.questions {
            out!(
                "  task {} {}{}",
                q.task_id,
                q.question,
                q.answer
                    .as_deref()
                    .map(|a| format!(" -> {a}"))
                    .unwrap_or_else(|| " (unanswered)".to_string())
            );
        }
    }
    for d in &doc.deployed {
        let sha = &d.sha[..d.sha.len().min(8)];
        let status = match d.check_ok {
            Some(true) => "ok".to_string(),
            Some(false) => match &d.rolled_back_to {
                Some(to) => format!("rolled back to {}", &to[..to.len().min(8)]),
                None => "failed".to_string(),
            },
            None => "running".to_string(),
        };
        out!(
            "deployed   task {} {} @ {sha} {status}",
            d.task_id,
            d.target
        );
        for fnd in &d.findings {
            out!("           {} {}", fnd.severity, fnd.finding);
        }
    }
    Ok(())
}
