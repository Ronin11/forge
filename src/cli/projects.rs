/// Arguments for `project_set`, kept together for one project set operation.
struct ProjectSet {
    name: String,
    purpose: Option<String>,
    workflow: Option<String>,
    per_task_usd: Option<f64>,
    per_initiative_usd: Option<f64>,
    supervisor_model: Option<String>,
    supervisor_per_lineage: Option<u32>,
    protected: Vec<String>,
    role: Vec<String>,
}

use super::initiatives::*;
use super::project_targets::*;
use super::*;

#[derive(Subcommand)]
pub(super) enum IntakeCmd {
    /// Accept a confirmed intake task's brief: create (or reuse) the
    /// project, fill its backlog with one entry per workflow the brief
    /// named, and record a draft deploy target from "where it runs" when
    /// it names a host and method Forge already supports (else a backlog
    /// entry saying what the target would be). Refused unless the task's
    /// brief says `confirmed`.
    Accept {
        task: i64,
        /// The project's name (default: the interviewed person's name, slugged)
        #[arg(long)]
        project: Option<String>,
        /// Register this repository to the project, and use it for the
        /// draft deploy target (default: the intake task's own repository)
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub(super) enum RefCmd {
    /// Record a reference on a task: the pull request it landed as, the issue it came from
    Add {
        task: i64,
        /// What kind of reference it is, e.g. "pr" or "issue"
        #[arg(long)]
        kind: String,
        #[arg(long)]
        url: String,
        /// Free text, e.g. the PR's title
        #[arg(long, default_value = "")]
        label: String,
        /// Who recorded it: "operator" by default, or a plugin's own name
        #[arg(long, default_value = "operator")]
        by: String,
    },
    /// A task's references
    List {
        task: i64,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum ProjectCmd {
    /// Register a new project: what is being built, and for whom
    New {
        name: String,
        /// One paragraph saying what the project is for
        #[arg(long)]
        purpose: String,
        /// A repository the project works in, optionally with the paths
        /// it owns there: `<path>` or `<path>:<scope1>,<scope2>` (repeatable)
        #[arg(long = "repo")]
        repos: Vec<String>,
    },
    /// Every project, its repositories, task counts by state, and cost
    List {
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// One project's repositories, task counts by state, and cost
    Show {
        name: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Set a project's defaults: what initiatives and tasks inherit
    /// unless they say otherwise (see docs/PROJECTS.md, "Defaults")
    Set {
        name: String,
        /// Replace the project's purpose paragraph (also clears the
        /// migration's placeholder, `Repository <path>.`)
        #[arg(long)]
        purpose: Option<String>,
        /// Which workflow a task in this project runs by default
        #[arg(long)]
        workflow: Option<String>,
        /// Default per-task cost cap in USD
        #[arg(long = "per-task-usd")]
        per_task_usd: Option<f64>,
        /// Default per-initiative cost cap in USD
        #[arg(long = "per-initiative-usd")]
        per_initiative_usd: Option<f64>,
        /// Default supervisor model
        #[arg(long = "supervisor-model")]
        supervisor_model: Option<String>,
        /// Default supervisor answers per lineage before a question reaches the operator
        #[arg(long = "supervisor-per-lineage")]
        supervisor_per_lineage: Option<u32>,
        /// Extra protected paths, on top of each repository's own forge.toml (repeatable)
        #[arg(long = "protected")]
        protected: Vec<String>,
        /// Which provider a role runs under in this project, as
        /// `<role>=<provider>` (role: code, tests, review, plan,
        /// supervisor; repeatable). Overrides the operator's [roles]
        /// table; a task's own --provider overrides this.
        #[arg(long = "role")]
        role: Vec<String>,
    },
    /// This project's backlog: things worth doing that are not yet queued
    Backlog {
        name: String,
        /// Add a backlog item
        #[arg(long)]
        add: Option<String>,
        /// Mark a backlog item done, by id
        #[arg(long)]
        done: Option<i64>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Deploy targets: where this project's landed code runs (see docs/DEPLOY.md)
    Deploy {
        #[command(subcommand)]
        cmd: ProjectDeployCmd,
    },
    /// Webhook tokens: what lets a caller outside Forge fire one of this
    /// project's webhook triggers (see docs/JOBS.md, "Triggers")
    Webhook {
        #[command(subcommand)]
        cmd: ProjectWebhookCmd,
    },
    /// Mint a fresh customer portal link for this project (see
    /// docs/PORTAL.md): prints "/p/<token>"
    Portal {
        name: String,
        /// Revoke every token minted earlier for this project, so only
        /// the fresh one keeps working
        #[arg(long)]
        revoke: bool,
    },
    /// Everything the customer portal's page needs for this project, in
    /// their own words: no ids, branches, costs, attempts or verdicts
    /// (see docs/PORTAL.md, "What they see")
    View {
        name: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Resolve a customer portal token to the project it opens (see
    /// docs/PORTAL.md, "What it is"): how the portal server turns
    /// `/p/<token>` into a project name before it calls `forge project
    /// view`. Exits non-zero if the token is unknown or revoked.
    ResolveToken {
        token: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

fn ref_add(task: i64, kind: String, url: String, label: String, by: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    if f.store.task(task)?.is_none() {
        bail!("no task {task}")
    }
    let id = f.store.insert_task_ref(task, &kind, &url, &label, &by)?;
    out!("{id} {kind} {url}");
    Ok(())
}

fn ref_list(task: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let rows: Vec<crate::view::RefRow> = f
        .store
        .task_refs(task)?
        .iter()
        .map(crate::view::RefRow::from)
        .collect();
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no references");
        return Ok(());
    }
    for r in &rows {
        out!(
            "{:<5} {:<8} {}{}",
            r.id,
            r.kind,
            r.url,
            if r.label.is_empty() {
                String::new()
            } else {
                format!("  {}", r.label)
            }
        );
    }
    Ok(())
}

fn print_project_row(r: &crate::view::ProjectRow) {
    out!("name       {}", r.name);
    out!("purpose    {}", r.purpose);
    out!("created_at {}", render::utc(r.created_at));
    if r.repos.is_empty() {
        out!("repos      none");
    }
    for repo in &r.repos {
        match &repo.scope {
            Some(scope) => out!("repo       {} ({scope})", repo.repo),
            None => out!("repo       {}", repo.repo),
        }
    }
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
    out!("cost       ${:.2}", r.cost_usd);
    out!(
        "jobs       today={} ok={} failed={} needs_human={} skipped={}",
        r.jobs_today,
        r.jobs_ok,
        r.jobs_failed,
        r.jobs_needs_human,
        r.jobs_skipped
    );
    out!(
        "defaults   workflow={} per-task=${} per-initiative=${} supervisor={} per-lineage={} protected={}",
        r.workflow.as_deref().unwrap_or("-"),
        r.per_task_usd
            .map_or("-".to_string(), |v| format!("{v:.2}")),
        r.per_initiative_usd
            .map_or("-".to_string(), |v| format!("{v:.2}")),
        r.supervisor_model.as_deref().unwrap_or("-"),
        r.supervisor_per_lineage
            .map_or("-".to_string(), |v| v.to_string()),
        if r.protected.is_empty() {
            "-".to_string()
        } else {
            r.protected.join(", ")
        }
    );
    if !r.role_providers.is_empty() {
        out!(
            "roles      {}",
            r.role_providers
                .iter()
                .map(|(role, provider)| format!("{role}={provider}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !r.proposals.is_empty() {
        out!("proposals");
        for p in &r.proposals {
            print_proposal_row(p);
        }
    }
}

fn print_proposal_row(p: &crate::view::ProposalRow) {
    out!(
        "  task {} quoting {} — {}",
        p.task_id,
        p.quoted
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        p.repetition
    );
    out!(
        "    outcome {}{}",
        p.outcome,
        match (p.answer.as_deref(), p.initiative) {
            (None, _) => " (pending)".to_string(),
            (Some(a), Some(iid)) => format!(" -> {a}, initiative {iid}"),
            (Some(a), None) => format!(" -> {a}"),
        }
    );
}

fn project_new(name: String, purpose: String, repos: Vec<String>) -> Result<()> {
    let f = Forge::open(false, false)?;
    if f.store.project(&name)?.is_some() {
        bail!("project {name} already exists");
    }
    f.store.create_project(&crate::store::Project {
        name: name.clone(),
        purpose,
        created_at: unix_now(),
        ..Default::default()
    })?;
    for r in repos {
        let (path, scope) = match r.split_once(':') {
            Some((p, s)) => (p, Some(s)),
            None => (r.as_str(), None),
        };
        let repo = Path::new(path)
            .canonicalize()
            .with_context(|| format!("--repo {path}"))?;
        let scope_json = scope
            .map(|s| serde_json::to_string(&s.split(',').collect::<Vec<_>>()))
            .transpose()?;
        f.store
            .register_repo(&name, &repo.display().to_string(), scope_json.as_deref())?;
    }
    out!("created project {name}");
    Ok(())
}

/// Parse `forge project set --role`'s `<role>=<provider>` pairs: the role
/// must be one of `config::ROLES`, and the provider must be configured.
/// `<role>=-` removes the role's pin, so the operator's `[roles]` (or the
/// experiment's draw, docs/ECONOMIST.md) applies to it again; until
/// 2026-09-22 a pin could be set and never cleared.
fn parse_role_providers(
    f: &Forge,
    role: &[String],
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    for pair in role {
        let (role, provider) = pair
            .split_once('=')
            .with_context(|| format!("--role {pair:?}: expected <role>=<provider>"))?;
        if !config::ROLES.contains(&role) {
            bail!(
                "--role {pair:?}: unknown role {role:?}; expected one of {}",
                config::ROLES.join(", ")
            );
        }
        if provider != "-" && !f.providers.contains_key(provider) {
            bail!("--role {pair:?}: unknown provider {provider:?}; see `forge providers`");
        }
        out.insert(role.to_string(), provider.to_string());
    }
    Ok(out)
}

fn project_set(args: ProjectSet) -> Result<()> {
    let ProjectSet {
        name,
        purpose,
        workflow,
        per_task_usd,
        per_initiative_usd,
        supervisor_model,
        supervisor_per_lineage,
        protected,
        role,
    } = args;
    let f = Forge::open(false, false)?;
    let role_providers = parse_role_providers(&f, &role)?;
    let d = crate::store::ProjectDefaults {
        purpose,
        workflow,
        per_task_usd,
        per_initiative_usd,
        supervisor_model,
        supervisor_per_lineage: supervisor_per_lineage.map(|v| v as i64),
        protected: (!protected.is_empty()).then_some(protected),
        role_providers,
    };
    if !f.store.set_project_defaults(&name, &d)? {
        bail!("no project {name}");
    }
    out!("updated project {name}");
    Ok(())
}

fn print_backlog_item(it: &crate::store::BacklogItem) {
    out!(
        "{:<5} {} {}",
        it.id,
        if it.done_at.is_some() { "done" } else { "open" },
        it.text
    );
}

fn project_backlog(name: String, add: Option<String>, done: Option<i64>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&name)?
        .with_context(|| format!("no project {name}"))?;
    if let Some(text) = add {
        let id = f.store.add_backlog(&name, &text)?;
        out!("added backlog item {id}");
    }
    if let Some(id) = done {
        if !f.store.mark_backlog_done(&name, id)? {
            bail!("no open backlog item {id} in project {name}");
        }
        out!("marked backlog item {id} done");
    }
    let items = f.store.backlog(&name)?;
    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(
                &items
                    .iter()
                    .map(|it| serde_json::json!({
                        "id": it.id,
                        "project": it.project,
                        "text": it.text,
                        "created_at": it.created_at,
                        "done_at": it.done_at,
                    }))
                    .collect::<Vec<_>>()
            )?
        );
        return Ok(());
    }
    if items.is_empty() {
        out!("no backlog items");
        return Ok(());
    }
    for it in &items {
        print_backlog_item(it);
    }
    Ok(())
}

/// Parse repeated `--arg <key>=<value>` flags into a map, in the order
/// clap collected them (last write wins on a repeated key).
fn intake_accept(task: i64, project: Option<String>, repo: Option<PathBuf>) -> Result<()> {
    let f = Forge::open(false, false)?;
    let repo = repo
        .map(|r| {
            r.canonicalize()
                .with_context(|| format!("--repo {}", r.display()))
        })
        .transpose()?
        .map(|r| r.display().to_string());
    let accepted = crate::intake::accept(&f, task, project, repo)?;
    for line in accepted.lines() {
        out!("{line}");
    }
    Ok(())
}

fn project_list(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let rows = crate::view::project_rows(&f)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no projects");
        return Ok(());
    }
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out!();
        }
        print_project_row(r);
    }
    Ok(())
}

fn project_show(name: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let p = f
        .store
        .project(&name)?
        .with_context(|| format!("no project {name}"))?;
    let row = crate::view::project_row(&f, &p)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&row)?);
        return Ok(());
    }
    print_project_row(&row);
    Ok(())
}

/// 32 bytes of OS randomness, hex-encoded: the same shape `forge-web`
/// mints its own access token in (`web/src/main.rs`'s `token`), and
/// already url-safe since every character is `0-9a-f`.
pub(super) fn random_portal_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .context("reading /dev/urandom")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn project_portal(name: String, revoke: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&name)?
        .with_context(|| format!("no project {name}"))?;
    if revoke {
        let n = f.store.revoke_portal_tokens(&name, unix_now())?;
        out!("revoked {n} earlier token(s) for project {name}");
    }
    let token = random_portal_token()?;
    f.store.create_portal_token(&name, &token, unix_now())?;
    out!("/p/{token}");
    Ok(())
}

fn project_view(name: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let p = f
        .store
        .project(&name)?
        .with_context(|| format!("no project {name}"))?;
    let doc = crate::view::portal_doc(&f, &p)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    out!("project    {}", doc.project);
    out!("purpose    {}", doc.purpose);
    out!();
    out!("Running for you:");
    for t in &doc.deploy_targets {
        out!(
            "  {:<12} {:<20} last deployed {} check={} look={}",
            t.name,
            t.where_it_runs,
            t.last_deployed_at
                .map(render::utc)
                .unwrap_or_else(|| "never".into()),
            t.check_ok
                .map(|ok| ok.to_string())
                .unwrap_or_else(|| "-".into()),
            t.look_ok
                .map(|ok| ok.to_string())
                .unwrap_or_else(|| "-".into()),
        );
    }
    out!();
    out!("Being built:");
    for i in &doc.initiatives {
        out!(
            "  [{}] {} ({} pieces of work)",
            i.state,
            i.outcome,
            i.pieces
        );
    }
    if doc.initiatives_more > 0 {
        out!("  ...and {} more", doc.initiatives_more);
    }
    out!();
    out!("Needs you:");
    for q in &doc.questions {
        out!("  #{} {}", q.task_id, q.text);
    }
    out!();
    out!("Done:");
    for l in &doc.landed {
        match l.pieces {
            Some(n) => out!(
                "  {} ({} pieces of work) ({})",
                l.text,
                n,
                render::utc(l.landed_at)
            ),
            None => out!("  {} ({})", l.text, render::utc(l.landed_at)),
        }
    }
    if doc.landed_more > 0 {
        out!("  ...and {} more", doc.landed_more);
    }
    out!();
    out!("Your plan:");
    if let Some(b) = &doc.brief {
        out!("  runs: {}", b.where_it_runs);
        for w in &b.workflows {
            out!("  - {w}");
        }
    }
    for b in &doc.backlog {
        out!("  backlog #{}: {}", b.id, b.text);
    }
    Ok(())
}

fn project_resolve_token(token: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let project = f
        .store
        .portal_token_project(&token)?
        .context("unknown or revoked token")?;
    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "project": project }))?
        );
        return Ok(());
    }
    out!("{project}");
    Ok(())
}

async fn dispatch_ref(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Ref { cmd } => match cmd {
            RefCmd::Add {
                task,
                kind,
                url,
                label,
                by,
            } => ref_add(task, kind, url, label, by),
            RefCmd::List { task, json } => ref_list(task, json),
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_project(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::New {
                name,
                purpose,
                repos,
            } => project_new(name, purpose, repos),
            ProjectCmd::List { json } => project_list(json),
            ProjectCmd::Show { name, json } => project_show(name, json),
            ProjectCmd::Set {
                name,
                purpose,
                workflow,
                per_task_usd,
                per_initiative_usd,
                supervisor_model,
                supervisor_per_lineage,
                protected,
                role,
            } => project_set(ProjectSet {
                name,
                purpose,
                workflow,
                per_task_usd,
                per_initiative_usd,
                supervisor_model,
                supervisor_per_lineage,
                protected,
                role,
            }),
            ProjectCmd::Backlog {
                name,
                add,
                done,
                json,
            } => project_backlog(name, add, done, json),
            ProjectCmd::Deploy { cmd } => dispatch_project_deploy(cmd),
            ProjectCmd::Webhook { cmd } => match cmd {
                ProjectWebhookCmd::Token {
                    project,
                    name,
                    trust,
                } => webhook_token(project, name, trust),
                ProjectWebhookCmd::Revoke { project, name } => webhook_revoke(project, name),
                ProjectWebhookCmd::List { project, json } => webhook_list(project, json),
            },
            ProjectCmd::Portal { name, revoke } => project_portal(name, revoke),
            ProjectCmd::View { name, json } => project_view(name, json),
            ProjectCmd::ResolveToken { token, json } => project_resolve_token(token, json),
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_initiative(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Initiative { cmd } => match cmd {
            InitiativeCmd::New {
                project,
                outcome,
                from,
                provider,
                workflow,
                budget,
                stop_after,
            } => {
                initiative_new(
                    project, outcome, from, provider, workflow, budget, stop_after,
                )
                .await
            }
            InitiativeCmd::FromPlan { task, outcome } => initiative_from_plan(task, outcome).await,
            InitiativeCmd::Set {
                id,
                budget,
                stop_after,
                outcome,
            } => initiative_set(id, budget, stop_after, outcome),
            InitiativeCmd::List { project, json } => initiative_list(project, json),
            InitiativeCmd::Show { id, json } => initiative_show(id, json),
            InitiativeCmd::Report { id, json } => initiative_report(id, json),
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_intake(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Intake { cmd } => match cmd {
            IntakeCmd::Accept {
                task,
                project,
                repo,
            } => intake_accept(task, project, repo),
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Ref { .. } => dispatch_ref(cmd).await,
        Cmd::Project { .. } => dispatch_project(cmd).await,
        Cmd::Initiative { .. } => dispatch_initiative(cmd).await,
        Cmd::Intake { .. } => dispatch_intake(cmd).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}
