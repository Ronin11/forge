use super::*;
use crate::workflows;

#[derive(Subcommand)]
pub(super) enum EconomistCmd {
    /// Shift `experiment.toml`'s weights toward the cheaper level of
    /// every factor, by the last N days of `forge stats --factors`,
    /// commit the result in the catalog's git, and exit non-zero (for
    /// `[limits] on_failure = "ask:operator"` to catch, see
    /// `.forge/workflows/economist-weekly.toml`) when any level's effect
    /// crosses --threshold
    Rebalance {
        /// Only tasks that finished in the last N days
        #[arg(long, default_value_t = 14)]
        days: i64,
        /// Print the weights it would write; never writes or commits
        #[arg(long)]
        dry_run: bool,
        /// A level whose |effect| (log true cost against its factor's
        /// reference level) exceeds this is a large move: asked about
        /// rather than shifted through quietly
        #[arg(long, default_value_t = 1.0)]
        threshold: f64,
    },
}

#[derive(Subcommand)]
pub(super) enum ExperimentCmd {
    /// Set one factor's weights directly — e.g. to apply the `forge
    /// experiment set ...` a rebalance's large-move question proposed
    /// (docs/ECONOMIST.md, "a large move is asked about before it
    /// compounds") — under the same validation `experiment::load` applies
    /// (role known, positive weights, normalized, none under the floor),
    /// then writes and commits `experiment.toml` in the catalog's own git.
    Set {
        /// The factor (role) to set, e.g. `review`
        factor: String,
        /// Each level's weight, as `<level>=<weight>` (repeatable)
        #[arg(value_name = "LEVEL=WEIGHT", required = true)]
        levels: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(super) enum MessageCmd {
    /// Record a message on a channel: `--from` for one that came from a
    /// contact, `--to` for one sent to them
    Record {
        /// The project the message is about
        project: String,
        /// The channel it was recorded on, e.g. "signal"
        #[arg(long)]
        channel: String,
        /// The contact it came from (an inbound message)
        #[arg(long, conflicts_with = "to")]
        from: Option<String>,
        /// The contact it was sent to (an outbound message)
        #[arg(long, conflicts_with = "from")]
        to: Option<String>,
        #[arg(long)]
        text: String,
        /// The task this message was about, if any
        #[arg(long)]
        task: Option<i64>,
    },
    /// A project's messages, newest first
    List {
        project: String,
        /// Only this contact's messages
        #[arg(long)]
        contact: Option<String>,
        /// Only messages at or after this unix second
        #[arg(long)]
        since: Option<i64>,
        /// Only messages in this direction: "in" or "out"
        #[arg(long)]
        direction: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum JobCmd {
    /// Start a job now, or leave it queued for the worker: a manual
    /// trigger through `forge job start <project> <workflow>` (see
    /// docs/JOBS.md, "The executor")
    Start {
        project: String,
        workflow: String,
        /// A JSON object; each top-level string field becomes
        /// `FORGE_INPUT_<NAME>` for every step, and the whole file is
        /// written to `$FORGE_INPUT_DIR/input.json`
        #[arg(long)]
        input: Option<PathBuf>,
        /// Record what each effect operation would do without doing it
        #[arg(long)]
        dry_run: bool,
        /// Run the job in this process now, rather than leaving it queued
        /// for the worker
        #[arg(long, conflicts_with_all = ["at", "delay"])]
        now: bool,
        /// Leave the job `scheduled` until this unix second, rather than
        /// queuing it immediately (docs/JOBS.md, "Delayed jobs")
        #[arg(long, conflicts_with = "delay")]
        at: Option<i64>,
        /// Leave the job `scheduled` until this long from now — a duration
        /// string (`s`, `m`, `h`, `d`), e.g. `5m` or `1h` — rather than
        /// queuing it immediately (docs/JOBS.md, "Delayed jobs")
        #[arg(long)]
        delay: Option<String>,
    },
    /// Fire a webhook: queue a job for the project's run workflow whose
    /// `[trigger]` is `on = "webhook"` with this name, refused without a
    /// valid `--token` from `forge project webhook token` (see
    /// docs/JOBS.md, "Triggers"). Prints the job's id; a delivery whose
    /// key was already fired prints the earlier job's id and starts no
    /// second job.
    Fire {
        project: String,
        /// The webhook's name: the `name` a workflow's `[trigger]` carries
        #[arg(long)]
        webhook: String,
        /// The delivery's body, a JSON object, as `forge job start
        /// --input` takes it (default: `{}`)
        #[arg(long)]
        input: Option<PathBuf>,
        /// The delivery's key: firing the same key again starts no second
        /// job (default: the SHA-256 of the input)
        #[arg(long = "ref")]
        reference: Option<String>,
        /// A token minted for this project's webhook
        #[arg(long)]
        token: Option<String>,
    },
    /// Withdraw a scheduled job before it becomes due: refused once it is
    /// queued, running, or finished
    Withdraw { id: i64 },
    /// Jobs, newest first, or only `<project>`'s
    List {
        project: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// One job, with every step and effect it recorded
    Show {
        id: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// A project's job effects across every one of its jobs, newest first
    Log {
        project: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Replay a run workflow's fixtures — every run workflow's in the
    /// repository when none is named — through the executor in dry-run
    /// mode, in a scratch directory, recording no job, and print each
    /// fixture as passed or by its first difference from what it expects:
    /// a missing effect, an extra one, a wrong state. Exits 1 on any
    /// difference, so a repository can declare `job-test = ["forge",
    /// "job", "test", "."]` among its checks (see docs/JOBS.md,
    /// "Verifying an automation")
    Test {
        /// A run workflow's name; a lone argument that names a directory
        /// is the path instead
        workflow: Option<String>,
        /// The repository whose `.forge/` holds the workflows and
        /// fixtures (default: the current directory)
        path: Option<PathBuf>,
    },
    /// Bench a run workflow's directive steps against every fixture under
    /// the project repository's `.forge/fixtures/<workflow>/`, once per
    /// named provider, in dry-run mode: schema-valid share, expected-kind
    /// share, mean cost and mean seconds, so the local model and the
    /// hosted ones are measured on the same real judgment (see
    /// docs/JOBS.md, "Steps")
    Bench {
        project: String,
        workflow: String,
        /// Provider names already configured under `[providers.<name>]`
        /// (see `forge providers`), comma-separated
        #[arg(long, value_delimiter = ',')]
        providers: Vec<String>,
    },
}

async fn message_record(
    project: String,
    channel: String,
    from: Option<String>,
    to: Option<String>,
    text: String,
    task: Option<i64>,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let (contact, direction) = match (from, to) {
        (Some(c), None) => (c, crate::store::Direction::In),
        (None, Some(c)) => (c, crate::store::Direction::Out),
        (Some(_), Some(_)) => bail!("--from and --to are mutually exclusive"),
        (None, None) => bail!("one of --from or --to is required"),
    };
    if let Some(id) = task
        && f.store.task(id)?.is_none()
    {
        bail!("no task {id}")
    }
    let id = f
        .store
        .insert_message(&project, &channel, &contact, direction, &text, task)?;
    out!("{id} {} {contact}", direction.as_str());
    // The trigger point (docs/JOBS.md, "Triggers"): an inbound message
    // queues a job for every run workflow whose `[trigger]` matches it. The
    // message is already recorded, so a start that fails is noted, not fatal.
    if direction == crate::store::Direction::In
        && let Some(m) = f.store.message(id)?
    {
        for (workflow, job) in crate::worker::message_triggers(&f, &m).await {
            eprintln!("job {job} queued ({workflow}, message {id})");
        }
    }
    Ok(())
}

fn message_list(
    project: String,
    contact: Option<String>,
    since: Option<i64>,
    direction: Option<String>,
    json: bool,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let direction = direction
        .as_deref()
        .map(crate::store::Direction::try_from)
        .transpose()?;
    let rows: Vec<crate::view::MessageRow> = f
        .store
        .messages(
            &project,
            &crate::store::MessageFilter {
                contact,
                since,
                direction,
            },
        )?
        .iter()
        .map(crate::view::MessageRow::from)
        .collect();
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no messages");
        return Ok(());
    }
    for m in &rows {
        out!(
            "{:<5} {:<3} {:<8} {:<12} {}",
            m.id,
            m.direction,
            m.channel,
            m.contact,
            m.text
        );
    }
    Ok(())
}

fn print_job_row(r: &crate::store::Job) {
    let sha = if r.landed_sha.is_empty() {
        "-".to_string()
    } else {
        r.landed_sha[..r.landed_sha.len().min(8)].to_string()
    };
    out!(
        "{:<5} {:<20} {:<12} {sha} {} {}{}",
        r.id,
        r.workflow,
        r.state.as_str(),
        r.trigger_kind,
        render::utc(r.started_at),
        r.due_at
            .map(|d| format!("  due {}", render::utc(d)))
            .unwrap_or_default()
    );
}

/// `forge job start <project> <workflow> [--input <file>] [--dry-run]
/// [--now | --at <unix> | --delay <duration>]` (see docs/JOBS.md, "The
/// executor" and "Delayed jobs").
async fn job_start(
    project: String,
    workflow: String,
    input: Option<PathBuf>,
    dry_run: bool,
    now: bool,
    at: Option<i64>,
    delay: Option<String>,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let due_at = match (at, delay) {
        (Some(at), _) => Some(at),
        (None, Some(d)) => {
            Some(unix_now() + workflows::parse_duration(&d).map_err(|e| anyhow::anyhow!(e))?)
        }
        (None, None) => None,
    };
    let id = crate::job::start(
        &f,
        &project,
        &workflow,
        input.as_deref(),
        dry_run,
        now,
        due_at,
    )
    .await?;
    out!("{id}");
    Ok(())
}

/// `forge job fire <project> --webhook <name> [--input <file>] [--ref
/// <key>] --token <token>` (docs/JOBS.md, "Triggers"). The token is checked
/// before anything else about the project or the hook is said, and every
/// way it can fail reads the same.
async fn job_fire(
    project: String,
    webhook: String,
    input: Option<PathBuf>,
    reference: Option<String>,
    token: Option<String>,
) -> Result<()> {
    let f = Forge::open(false, false)?;
    let level = match token.as_deref() {
        Some(t) if !t.is_empty() => f.store.webhook_token_trust(
            &project,
            &webhook,
            &crate::job::sha256_hex(t.as_bytes()),
        )?,
        _ => None,
    };
    let Some(level) = level else {
        bail!(
            "invalid webhook token for {project}/{webhook}: pass --token from `forge project webhook token`; a revoked, unknown or missing token is refused"
        );
    };
    let bytes = match input.as_deref() {
        Some(p) => std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
        None => b"{}".to_vec(),
    };
    let trigger_ref = match reference {
        Some(r) if r.trim().is_empty() || r.len() > 200 => {
            bail!("--ref must be 1 to 200 characters")
        }
        Some(r) => r,
        None => crate::job::sha256_hex(&bytes),
    };
    let input_text = String::from_utf8(bytes).context("the input file must be UTF-8")?;
    let (workflow, wf, source, landed_sha) =
        worker::webhook_workflow(&f, &project, &webhook).await?;
    let (id, started) = crate::job::start_webhook(
        &f,
        &project,
        &workflow,
        &landed_sha,
        &wf,
        source,
        &trigger_ref,
        &input_text,
    )?;
    if started {
        f.store.set_job_trust(id, level)?;
    } else {
        eprintln!("job {id} already started for ref {trigger_ref}; nothing new started");
    }
    out!("{id}");
    Ok(())
}

/// `forge job withdraw <id>` (see docs/JOBS.md, "Delayed jobs").
fn job_withdraw(id: i64) -> Result<()> {
    let f = Forge::open(false, false)?;
    if !f.store.withdraw_job(id)? {
        bail!("job {id} is not scheduled; only a scheduled job, not yet due, can be withdrawn");
    }
    out!("withdrew job {id}");
    Ok(())
}

/// `forge job list [<project>] [--json]`: jobs, newest first, or only
/// `<project>`'s (see docs/JOBS.md, "The record").
fn job_list(project: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    if let Some(p) = &project {
        f.store
            .project(p)?
            .with_context(|| format!("no project {p}"))?;
    }
    let rows = f.store.jobs(project.as_deref(), None)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no jobs");
        return Ok(());
    }
    for r in &rows {
        print_job_row(r);
    }
    Ok(())
}

/// `forge job show <id> [--json]`: one job, with every step and effect it recorded.
fn job_show(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let j = f.store.job(id)?.with_context(|| format!("no job {id}"))?;
    let doc = crate::view::job_doc(&f, &j)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    out!("job {} ({})", doc.id, doc.project);
    out!("workflow   {} ({})", doc.workflow, doc.workflow_source);
    out!("trigger    {} {}", doc.trigger_kind, doc.trigger_ref);
    if let Some(t) = f.store.job_trust(id)? {
        out!("trust      {}", t.as_str());
    }
    out!(
        "state      {}{}",
        doc.state,
        if doc.dry_run { " (dry run)" } else { "" }
    );
    if let Some(due) = doc.due_at {
        out!("due        {}", render::utc(due));
    }
    out!("cost       ${:.2}", doc.cost_usd.unwrap_or(0.0));
    if doc.state == "skipped" {
        let verdict: Vec<crate::checks::CheckResult> =
            serde_json::from_str(&doc.verdict_json).unwrap_or_default();
        if let Some(c) = verdict.first() {
            out!("reason     {}", c.tail);
        }
    }
    if !doc.steps.is_empty() {
        out!("steps");
        let verdict: Vec<crate::checks::CheckResult> =
            serde_json::from_str(&doc.verdict_json).unwrap_or_default();
        for s in &doc.steps {
            out!("  {:<3} {:<20} {}", s.seq, s.action, s.kind);
            if s.kind == "directive" {
                let log = f.paths.logs.join(format!("job-{}-{}.jsonl", doc.id, s.seq));
                if log.exists() {
                    out!("      log    {}", log.display());
                }
            }
            if s.kind == "operation" {
                if let Some(code) = s.exit_code {
                    out!("      exit   {code}");
                }
                for line in s.tail.lines() {
                    out!("      | {line}");
                }
                if !s.output_ref.is_empty() {
                    out!("      output {}", s.output_ref);
                }
            } else if let Some(c) = verdict.iter().find(|c| c.name == s.action && !c.ok) {
                out!("      failed {}", c.tail);
            }
        }
    }
    if !doc.effects.is_empty() {
        out!("effects");
        for e in &doc.effects {
            out!("  {:<3} {:<10} {} {}", e.seq, e.kind, e.target, e.summary);
        }
    }
    Ok(())
}

/// `forge job log <project> [--json]`: a project's job effects across
/// every one of its jobs, newest first.
fn job_log(project: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let rows = f.store.job_effects_for_project(&project)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no job effects");
        return Ok(());
    }
    for r in &rows {
        out!(
            "{:<5} job {:<5} {:<10} {} {}{}",
            r.id,
            r.job_id,
            r.kind,
            r.target,
            r.summary,
            if r.dry_run { " (dry run)" } else { "" }
        );
    }
    Ok(())
}

/// `forge job test [<workflow>] [<path>]` reads `forge job test .` as the
/// path: a lone argument is a workflow only when this directory holds a
/// run workflow of that name, or when it names no directory and holds no
/// path separator.
#[rustfmt::skip]
pub(super) fn job_test_target(workflow: Option<String>, path: Option<PathBuf>) -> (Option<String>, PathBuf) {
    match (workflow, path) {
        (Some(w), None) => {
            let named_here = Path::new(".forge/workflows")
                .join(format!("{w}.toml"))
                .exists();
            let is_path = w == "." || w == ".." || w.contains('/') || Path::new(&w).is_dir();
            if is_path && !named_here {
                (None, PathBuf::from(w))
            } else {
                (Some(w), PathBuf::from("."))
            }
        }
        (w, p) => (w, p.unwrap_or_else(|| PathBuf::from("."))),
    }
}

/// `forge job test [<workflow>] [<path>]`: see `crate::job::test`. Every
/// fixture is printed, and any difference ends the command in an error,
/// which is the exit status a repository check reads.
async fn job_test(workflow: Option<String>, path: Option<PathBuf>) -> Result<()> {
    let (workflow, root) = job_test_target(workflow, path);
    let outcomes = crate::job::test(&root, workflow.as_deref()).await?;
    let mut failed = Vec::new();
    for o in &outcomes {
        let label = if o.name.is_empty() {
            o.workflow.clone()
        } else {
            format!("{}/{}", o.workflow, o.name)
        };
        match o.differences.split_first() {
            None => out!("pass  {label}"),
            Some((first, rest)) => {
                out!("FAIL  {label}: {first}");
                for d in rest {
                    out!("        and {d}");
                }
                failed.push(format!("{label}: {first}"));
            }
        }
    }
    out!(
        "{} fixture(s): {} passed, {} failed",
        outcomes.len(),
        outcomes.len() - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        anyhow::bail!(
            "{} of {} fixture(s) differ from what they expect:\n{}",
            failed.len(),
            outcomes.len(),
            failed.join("\n")
        );
    }
    Ok(())
}

/// `forge job bench <project> <workflow> --providers a,b`: see
/// `crate::job::bench`.
async fn job_bench(project: String, workflow: String, providers: Vec<String>) -> Result<()> {
    if providers.is_empty() {
        anyhow::bail!("--providers needs at least one name, comma-separated");
    }
    let f = Forge::open(false, false)?;
    let rows = crate::job::bench(&f, &project, &workflow, &providers).await?;
    out!(
        "{:<12} {:>4}  {:<16} {:<16} {:>10} {:>12}",
        "provider",
        "runs",
        "schema-valid",
        "expected-kind",
        "mean-cost",
        "mean-seconds"
    );
    for r in &rows {
        let pct = |n: usize| {
            if r.runs == 0 {
                0.0
            } else {
                100.0 * n as f64 / r.runs as f64
            }
        };
        let mean = |x: f64| if r.runs == 0 { 0.0 } else { x / r.runs as f64 };
        out!(
            "{:<12} {:>4}  {:<16} {:<16} {:>10} {:>12}",
            r.provider,
            r.runs,
            format!(
                "{}/{} ({:.0}%)",
                r.schema_valid,
                r.runs,
                pct(r.schema_valid)
            ),
            format!(
                "{}/{} ({:.0}%)",
                r.kind_correct,
                r.runs,
                pct(r.kind_correct)
            ),
            format!("${:.4}", mean(r.cost_usd)),
            format!("{:.2}s", mean(r.seconds)),
        );
    }
    Ok(())
}

async fn ask(project: String, message: String, from: Option<String>) -> Result<()> {
    let f = Arc::new(Forge::open(true, false)?);
    let (asked, proposal) =
        crate::concierge::ask(f.clone(), &project, &message, from.as_deref()).await?;
    match asked {
        crate::concierge::Asked::Filed { task } => {
            out!(
                "concierge: a request; filed task {task} ({} queued)",
                f.store.queued_count()?
            );
        }
        crate::concierge::Asked::Answered { answer, decision } => {
            out!("{answer}");
            eprintln!("concierge: a question; recorded as decision {decision}");
        }
        crate::concierge::Asked::Need { task, reason } => {
            out!("concierge: a need ({reason}); filed intake task {task}");
        }
        crate::concierge::Asked::Unclear { task, question } => {
            out!(
                "concierge: unclear; blocked task {task} with a question{}: {question}",
                from.as_deref()
                    .map(|c| format!(" for {c}"))
                    .unwrap_or_default()
            );
        }
    }
    if let Some(task) = proposal {
        out!(
            "concierge: also proposes an automation; blocked task {task} with a question{}",
            from.as_deref()
                .map(|c| format!(" for {c}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

/// `forge economist rebalance` (docs/ECONOMIST.md, "What is built"): the
/// weekly step `.forge/workflows/economist-weekly.toml` runs. Reads the
/// same numbers `forge stats --factors --json` prints
/// (`Store::factor_stats`) over the last `days`, shifts `experiment.toml`'s
/// weights toward the cheaper, more confidently-measured level of every
/// factor it declares, except a factor holding a level whose effect
/// crosses `threshold`, which is left exactly as declared
/// (`experiment::rebalance`), and — unless `dry_run` — writes and commits
/// the result in the catalog's own git (`git::commit_path`), with a
/// message naming what moved (`experiment::commit_message`). Exits
/// non-zero when any level's effect crosses `threshold`, dry run or not,
/// naming that level and, per held factor, the proposed weights as a
/// `forge experiment set` invocation to apply them by hand
/// (`experiment::large_move_message`), so a real run's `[limits]
/// on_failure = "ask:operator"` catches it — a dry run never asks, since
/// the job driver only honours `on_failure` outside dry runs.
async fn economist_rebalance(days: i64, dry_run: bool, threshold: f64) -> Result<()> {
    let f = Forge::open(false, false)?;
    let catalog = workflows::catalog_dir(&f.paths.home)?;
    let Some(exp) = crate::experiment::load(&catalog)? else {
        out!(
            "no experiment.toml in {}; nothing to rebalance",
            catalog.display()
        );
        return Ok(());
    };
    let since = unix_now() - days * 86_400;
    let stats = f
        .store
        .factor_stats(&crate::store::StatsFilter::default(), Some(since))?;
    let result = crate::experiment::rebalance(&exp.factors, exp.floor, &stats, threshold);
    if result.shifts.is_empty() {
        out!("no factor's weights moved");
    }
    for shift in &result.shifts {
        out!("{}:", shift.factor);
        for (level, w) in &shift.after {
            let before = shift.before.get(level).copied().unwrap_or(0.0);
            out!("  {level:<16} {before:.3} -> {w:.3}");
        }
    }
    if !dry_run {
        let new_exp = crate::experiment::ExperimentFile {
            floor: exp.floor,
            factors: result.factors.clone(),
        };
        if new_exp.factors != exp.factors {
            crate::experiment::save(&catalog, &new_exp)?;
            let message = crate::experiment::commit_message(&result.shifts);
            match git::commit_path(&catalog, "experiment.toml", &message).await? {
                Some(hash) => out!("{hash}"),
                None => out!("weights unchanged; nothing to commit"),
            }
        } else {
            out!("weights unchanged; nothing to commit");
        }
    }
    if !result.large_moves.is_empty() {
        bail!(crate::experiment::large_move_message(
            &result.large_moves,
            &result.held,
            threshold
        ));
    }
    Ok(())
}

/// `forge experiment set <factor> <level>=<weight>...`: applies a
/// rebalance's held proposal by hand (docs/ECONOMIST.md, "a large move is
/// asked about before it compounds") — the same validation `experiment::load`
/// applies to every factor in the file, then writes and commits
/// `experiment.toml` in the catalog's own git, same as a normal rebalance.
async fn experiment_set(factor: String, levels: Vec<String>) -> Result<()> {
    let f = Forge::open(false, false)?;
    let catalog = workflows::catalog_dir(&f.paths.home)?;
    let current = crate::experiment::load(&catalog)?;
    let mut weights = BTreeMap::new();
    for kv in &levels {
        let (level, w) = kv
            .split_once('=')
            .with_context(|| format!("{kv:?}: expected <level>=<weight>"))?;
        let w: f64 = w
            .trim()
            .parse()
            .with_context(|| format!("{kv:?}: weight must be a number"))?;
        weights.insert(level.trim().to_string(), w);
    }
    let before = current
        .as_ref()
        .and_then(|e| e.factors.get(&factor))
        .cloned()
        .unwrap_or_default();
    let new_exp = crate::experiment::set_factor(&catalog, current, &factor, weights)?;
    let after = new_exp.factors[&factor].clone();
    crate::experiment::save(&catalog, &new_exp)?;
    let message = crate::experiment::commit_message(&[crate::experiment::FactorShift {
        factor,
        before,
        after,
    }]);
    match git::commit_path(&catalog, "experiment.toml", &message).await? {
        Some(hash) => out!("{hash}"),
        None => out!("weights unchanged; nothing to commit"),
    }
    Ok(())
}

async fn dispatch_ask(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Ask {
            project,
            message,
            from,
        } => ask(project, message, from).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_message(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Message { cmd } => match cmd {
            MessageCmd::Record {
                project,
                channel,
                from,
                to,
                text,
                task,
            } => message_record(project, channel, from, to, text, task).await,
            MessageCmd::List {
                project,
                contact,
                since,
                direction,
                json,
            } => message_list(project, contact, since, direction, json),
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_job(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Job { cmd } => match cmd {
            JobCmd::Start {
                project,
                workflow,
                input,
                dry_run,
                now,
                at,
                delay,
            } => job_start(project, workflow, input, dry_run, now, at, delay).await,
            JobCmd::Fire {
                project,
                webhook,
                input,
                reference,
                token,
            } => job_fire(project, webhook, input, reference, token).await,
            JobCmd::Withdraw { id } => job_withdraw(id),
            JobCmd::List { project, json } => job_list(project, json),
            JobCmd::Show { id, json } => job_show(id, json),
            JobCmd::Log { project, json } => job_log(project, json),
            JobCmd::Test { workflow, path } => job_test(workflow, path).await,
            JobCmd::Bench {
                project,
                workflow,
                providers,
            } => job_bench(project, workflow, providers).await,
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_economist(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Economist { cmd } => match cmd {
            EconomistCmd::Rebalance {
                days,
                dry_run,
                threshold,
            } => economist_rebalance(days, dry_run, threshold).await,
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_experiment(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Experiment { cmd } => match cmd {
            ExperimentCmd::Set { factor, levels } => experiment_set(factor, levels).await,
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Ask { .. } => dispatch_ask(cmd).await,
        Cmd::Message { .. } => dispatch_message(cmd).await,
        Cmd::Job { .. } => dispatch_job(cmd).await,
        Cmd::Economist { .. } => dispatch_economist(cmd).await,
        Cmd::Experiment { .. } => dispatch_experiment(cmd).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}
