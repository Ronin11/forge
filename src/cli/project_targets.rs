/// Arguments for `project_deploy_add`, kept together for one project deploy add operation.
struct ProjectDeployAdd {
    project: String,
    name: String,
    repo: PathBuf,
    scope: Option<String>,
    method: String,
    args: Vec<String>,
    check: Option<String>,
    smoke: Option<String>,
    on_landing: bool,
}

/// Arguments for `project_deploy_set`, kept together for one project deploy set operation.
struct ProjectDeploySet {
    project: String,
    name: String,
    repo: Option<PathBuf>,
    scope: Option<String>,
    method: Option<String>,
    args: Vec<String>,
    check: Option<String>,
    smoke: Option<String>,
    on_landing: bool,
    no_on_landing: bool,
}

use super::projects::random_portal_token;
use super::*;

#[derive(Subcommand)]
pub(super) enum ProjectWebhookCmd {
    /// Mint a token for a project's webhook `<name>` and print it once:
    /// only its hash is kept, and it fires that one hook, nothing else
    Token {
        project: String,
        name: String,
        /// Trust level a delivery under this token carries: operator,
        /// contact, or public (default public: the caller is outside Forge)
        #[arg(long, default_value = "public")]
        trust: String,
    },
    /// Revoke every active token on a project's webhook `<name>`
    Revoke { project: String, name: String },
    /// A project's webhook tokens (never the tokens themselves): which
    /// hook, when minted, and whether revoked
    List {
        project: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum ProjectDeployCmd {
    /// Declare a deploy target: where landed code runs, how it gets
    /// there, and what proves it is up
    Add {
        project: String,
        name: String,
        /// The repository this target deploys
        #[arg(long)]
        repo: PathBuf,
        /// Paths within the repository this target owns, comma-separated
        /// (default: the whole repository)
        #[arg(long)]
        scope: Option<String>,
        /// The action file this target runs, e.g. "deploy-command"
        #[arg(long)]
        method: String,
        /// An argument to the method, as `<key>=<value>` (repeatable)
        #[arg(long = "arg")]
        args: Vec<String>,
        /// A shell command, run where the thing runs, whose exit status
        /// is the deploy's verdict. Required, except for deploy-static,
        /// which defaults to fetching a url arg and requiring 200, and
        /// deploy-self, which defaults to fetching the web client's /tasks
        #[arg(long)]
        check: Option<String>,
        /// After the check passes, open this url in headless Chromium and
        /// fail the deploy on a console error or a failed request to its
        /// own origin (see docs/DEPLOY.md, "A deterministic smoke step")
        #[arg(long)]
        smoke: Option<String>,
        /// Run this target automatically after a landing on its repository
        #[arg(long)]
        on_landing: bool,
    },
    /// A project's deploy targets
    List {
        project: String,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Change a deploy target's fields, replacing only the ones given: an
    /// `--arg` replaces or adds that key, leaving the others; `--check`,
    /// `--smoke` and `--on-landing`/`--no-on-landing` replace their field
    /// the same way
    Set {
        project: String,
        name: String,
        /// The repository this target deploys
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Paths within the repository this target owns, comma-separated
        #[arg(long)]
        scope: Option<String>,
        /// The action file this target runs, e.g. "deploy-command"
        #[arg(long)]
        method: Option<String>,
        /// An argument to the method, as `<key>=<value>` (repeatable);
        /// replaces or adds that key, leaving the others as they were
        #[arg(long = "arg")]
        args: Vec<String>,
        /// A shell command, run where the thing runs, whose exit status
        /// is the deploy's verdict
        #[arg(long)]
        check: Option<String>,
        /// After the check passes, open this url in headless Chromium and
        /// fail the deploy on a console error or a failed request to its
        /// own origin (see docs/DEPLOY.md, "A deterministic smoke step")
        #[arg(long)]
        smoke: Option<String>,
        /// Run this target automatically after a landing on its repository
        #[arg(long)]
        on_landing: bool,
        /// Stop running this target automatically after a landing
        #[arg(long, conflicts_with = "on_landing")]
        no_on_landing: bool,
    },
    /// Remove a deploy target; refused while a deploy of it is running
    Remove { project: String, name: String },
}

pub(super) fn parse_args(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for pair in pairs {
        let (k, v) = pair
            .split_once('=')
            .with_context(|| format!("--arg {pair:?}: expected <key>=<value>"))?;
        map.insert(k.to_string(), v.to_string());
    }
    Ok(map)
}

fn project_deploy_add(args: ProjectDeployAdd) -> Result<()> {
    let ProjectDeployAdd {
        project,
        name,
        repo,
        scope,
        method,
        args,
        check,
        smoke,
        on_landing,
    } = args;
    let f = Forge::open(false, false)?;
    let t = crate::deploy::add_target(
        &f,
        crate::deploy::TargetSpec {
            project,
            name,
            repo,
            scope,
            method,
            args,
            check,
            smoke,
            on_landing,
        },
    )?;
    out!("added deploy target {} to project {}", t.name, t.project);
    Ok(())
}

fn project_deploy_set(args: ProjectDeploySet) -> Result<()> {
    let ProjectDeploySet {
        project,
        name,
        repo,
        scope,
        method,
        args,
        check,
        smoke,
        on_landing,
        no_on_landing,
    } = args;
    let f = Forge::open(false, false)?;
    let t = crate::deploy::set_target(
        &f,
        &project,
        &name,
        crate::deploy::TargetChanges {
            repo,
            scope,
            method,
            args,
            check,
            smoke,
            on_landing,
            no_on_landing,
        },
    )?;
    out!("updated deploy target {} in project {}", t.name, t.project);
    Ok(())
}

/// Remove a deploy target: no more `--on-landing` runs for it, and it
/// disappears from `forge project deploy list` and `forge deploy`.
/// Refused while a deploy of it is running (a `deploys` row with no
/// `finished_at` yet); the deploy history itself is untouched.
fn project_deploy_remove(project: String, name: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .deploy_target(&project, &name)?
        .with_context(|| format!("no deploy target {name} in project {project}"))?;
    let running = f
        .store
        .deploys(&project, Some(&name))?
        .iter()
        .any(|d| d.finished_at.is_none());
    if running {
        bail!("deploy target {name} in project {project} has a deploy running");
    }
    f.store.remove_deploy_target(&project, &name)?;
    out!("removed deploy target {name} from project {project}");
    Ok(())
}

fn print_deploy_target_row(t: &crate::store::DeployTarget) {
    out!(
        "{:<12} repo={} method={}{} on_landing={}",
        t.name,
        t.repo,
        t.method,
        t.scope
            .as_ref()
            .map(|s| format!(" scope={s}"))
            .unwrap_or_default(),
        t.on_landing
    );
    out!("{:<12} check={}", "", t.check_cmd);
    if let Some(url) = &t.smoke_url {
        out!("{:<12} smoke={}", "", url);
    }
}

fn project_deploy_list(project: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let rows = f.store.deploy_targets(&project)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no deploy targets");
        return Ok(());
    }
    for r in &rows {
        print_deploy_target_row(r);
    }
    Ok(())
}

/// `forge project webhook token <project> <name>`: mint a token for a
/// webhook and print it — the only time it is shown.
pub(super) fn webhook_token(project: String, name: String, trust: String) -> Result<()> {
    let trust = crate::store::Trust::try_from(trust.as_str())?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("a webhook name is letters, digits, '-', '_' and '.' (it is a URL path segment)");
    }
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let token = random_portal_token()?;
    f.store.create_webhook_token(
        &project,
        &name,
        &crate::job::sha256_hex(token.as_bytes()),
        trust,
        unix_now(),
    )?;
    out!("{token}");
    Ok(())
}

/// `forge project webhook revoke <project> <name>`.
pub(super) fn webhook_revoke(project: String, name: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let n = f.store.revoke_webhook_tokens(&project, &name, unix_now())?;
    out!("revoked {n} token(s) for webhook {name} of project {project}");
    Ok(())
}

/// `forge project webhook list <project> [--json]`.
pub(super) fn webhook_list(project: String, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let rows = f.store.webhook_tokens(&project)?;
    if json {
        let v: Vec<_> = rows
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "project": t.project,
                    "name": t.name,
                    "trust": t.trust.as_str(),
                    "created_at": t.created_at,
                    "revoked_at": t.revoked_at,
                })
            })
            .collect();
        out!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    for t in rows {
        match t.revoked_at {
            Some(at) => out!(
                "{:<20} minted {} revoked {}",
                t.name,
                render::utc(t.created_at),
                render::utc(at)
            ),
            None => out!("{:<20} minted {} active", t.name, render::utc(t.created_at)),
        }
    }
    Ok(())
}

pub(super) fn dispatch_project_deploy(cmd: ProjectDeployCmd) -> Result<()> {
    match cmd {
        ProjectDeployCmd::Add {
            project,
            name,
            repo,
            scope,
            method,
            args,
            check,
            smoke,
            on_landing,
        } => project_deploy_add(ProjectDeployAdd {
            project,
            name,
            repo,
            scope,
            method,
            args,
            check,
            smoke,
            on_landing,
        }),
        ProjectDeployCmd::List { project, json } => project_deploy_list(project, json),
        ProjectDeployCmd::Set {
            project,
            name,
            repo,
            scope,
            method,
            args,
            check,
            smoke,
            on_landing,
            no_on_landing,
        } => project_deploy_set(ProjectDeploySet {
            project,
            name,
            repo,
            scope,
            method,
            args,
            check,
            smoke,
            on_landing,
            no_on_landing,
        }),
        ProjectDeployCmd::Remove { project, name } => project_deploy_remove(project, name),
    }
}
