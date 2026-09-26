use super::project_targets::parse_args;
use super::*;

#[derive(Args)]
pub struct ProvisionArgs {
    /// The project the deploy target belongs to
    project: String,
    /// The deploy target to provision a box for
    name: String,
    /// An argument to the provision-hetzner operation, as `<key>=<value>`
    /// (repeatable): type, location, image, cloud_init, ssh_keys
    #[arg(long = "arg")]
    args: Vec<String>,
}

#[derive(Args)]
pub struct DeployArgs {
    #[command(subcommand)]
    cmd: Option<DeploySub>,
    /// The project the target belongs to (omit only with `log`)
    project: Option<String>,
    /// The target to deploy (omit only with `log`)
    name: Option<String>,
    /// The commit to deploy (default: the tip of the repository's base
    /// branch; for a deploy-self target, origin's tip)
    #[arg(long)]
    sha: Option<String>,
    /// For a deploy-self target: deploy a commit older than the live
    /// release anyway
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand)]
pub(super) enum DeploySub {
    /// What was deployed when, and what the check said
    Log {
        project: String,
        /// Only this target's deploys
        name: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum PluginCmd {
    /// Every plugin found, where it came from, enabled or not
    List {
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Whether a plugin (or every plugin) is enabled
    Status {
        name: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Enable a plugin: a running `forge work` notices within a few
    /// seconds and starts it, no restart needed
    Enable { name: String },
    /// Disable a plugin: a running `forge work` notices within a few
    /// seconds and stops it, no restart needed
    Disable { name: String },
    /// Reload an enabled plugin's config: a running `forge work` notices
    /// within a few seconds and replaces its process with a fresh one
    /// that re-reads FORGE_PLUGIN_DIR/config. Unlike enable/disable,
    /// this is the way to pick up a config edit without ever changing
    /// whether the plugin is enabled.
    Restart { name: String },
    /// Copy a plugin directory into FORGE_HOME/plugins and run its build
    Install {
        /// The plugin's own directory, holding plugin.toml
        path: PathBuf,
    },
    /// Stop a plugin, clear its enabled flag, and remove the installed
    /// copy; its FORGE_HOME/plugins-state is left alone
    Uninstall { name: String },
    /// A plugin's stdout/stderr log
    Logs {
        name: String,
        /// Keep printing as the log grows
        #[arg(long, short)]
        follow: bool,
    },
}

/// Run a deploy target now (see docs/DEPLOY.md, "When a deploy runs").
async fn deploy_run(
    project: Option<String>,
    name: Option<String>,
    sha: Option<String>,
    force: bool,
) -> Result<()> {
    let (project, name) = match (project, name) {
        (Some(p), Some(n)) => (p, n),
        _ => bail!("usage: forge deploy <project> <name> [--sha <commit>] [--force]"),
    };
    let f = Forge::open(false, false)?;
    if !crate::deploy::run(&f, &project, &name, sha, None, force).await? {
        std::process::exit(1);
    }
    Ok(())
}

fn print_deploy_row(r: &crate::store::Deploy) {
    let sha = &r.sha[..r.sha.len().min(8)];
    let status = match r.check_ok {
        Some(true) => "ok".to_string(),
        Some(false) => match &r.rolled_back_to {
            Some(to) => format!("FAILED, rolled back to {}", &to[..to.len().min(8)]),
            None => "FAILED".to_string(),
        },
        None => "running".to_string(),
    };
    out!(
        "{:<5} {:<12} {sha} {status} {}",
        r.id,
        r.target,
        render::utc(r.started_at)
    );
    if !r.reason.is_empty() {
        out!("{:<19}{}", "", r.reason);
    }
    if let Some(ok) = r.smoke_ok {
        out!("{:<19}smoke {}", "", if ok { "ok" } else { "FAILED" });
    }
    if let Some(ok) = r.look_ok {
        out!("{:<19}look  {}", "", if ok { "ok" } else { "FAILED" });
    }
    let findings: Vec<crate::deploy_look::Finding> = r
        .look_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();
    for fnd in &findings {
        out!("{:<19}  {} {}", "", fnd.severity, fnd.finding);
    }
}

fn deploy_log(project: String, name: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let rows = f.store.deploys(&project, name.as_deref())?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no deploys");
        return Ok(());
    }
    for r in &rows {
        print_deploy_row(r);
    }
    Ok(())
}

/// `forge provision <project> <name> [--arg k=v]...`: run
/// `provision-hetzner` for the deploy target already declared as `name` in
/// `project` (see docs/DEPLOY.md, "Provisioning"), and record its ipv4 as
/// that target's `host` arg. `type`, `location` and `image` default as the
/// operation itself does when not given here; `cloud_init` and `ssh_keys`
/// have no default and must be given.
const PROVISION_TIMEOUT: Duration = Duration::from_secs(900);

async fn provision_run(project: String, name: String, args: Vec<String>) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store
        .project(&project)?
        .with_context(|| format!("no project {project}"))?;
    let mut target = f
        .store
        .deploy_target(&project, &name)?
        .with_context(|| format!("no deploy target {name} in project {project}"))?;
    let action = operation::resolve_provision(&f)?;

    let mut arg_map = parse_args(&args)?;
    arg_map
        .entry("type".to_string())
        .or_insert_with(|| "cpx21".to_string());
    arg_map
        .entry("location".to_string())
        .or_insert_with(|| "ash".to_string());
    arg_map
        .entry("image".to_string())
        .or_insert_with(|| "debian-12".to_string());
    if !arg_map.contains_key("cloud_init") {
        bail!("--arg cloud_init=<path> is required");
    }
    arg_map.insert("name".to_string(), name.clone());

    let out_dir = f.paths.home.join("provision").join(&project).join(&name);
    let r = operation::run_provision(&action, &arg_map, &out_dir, PROVISION_TIMEOUT).await?;
    if !r.ok {
        out!("{}", r.tail);
        bail!("provision-hetzner failed for {name} in project {project}");
    }
    let ipv4 = r
        .stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .with_context(|| "provision-hetzner printed no ipv4 address")?
        .trim()
        .to_string();

    target.args.insert("host".to_string(), ipv4.clone());
    f.store.update_deploy_target(&target)?;

    let ssh_config = out_dir.join("ssh-config");
    out!("provisioned {name} in project {project}: {ipv4}");
    out!(
        "updated deploy target {name}'s host arg to {ipv4}; ssh config fragment written to {} (append it to ~/.ssh/config)",
        ssh_config.display()
    );
    Ok(())
}

fn plugin_list(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let (rows, problems) = crate::view::plugin_rows(&f)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let home_cfg = config::load_home(&f.paths.home)?;
    let cat = crate::plugins::load_catalog(&f.paths.home, &home_cfg.plugin_dirs);
    for r in &rows {
        out!(
            "{:<16} {:<8} {:<10} {:<16} {}",
            r.name,
            if r.enabled { "enabled" } else { "disabled" },
            r.restart,
            r.capabilities.join(","),
            r.dir,
        );
        if !r.description.is_empty() {
            out!("             {}", r.description);
        }
        if let Some(p) = cat.plugins.get(&r.name) {
            out!("             runs       {}", p.manifest.run.join(" "));
            if let Some(b) = &p.manifest.build {
                out!("             build      {}", b.join(" "));
            }
        }
    }
    for p in &problems {
        out!("problem: {} {}", p.file, p.what);
    }
    Ok(())
}

fn plugin_status(name: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let (rows, _) = crate::view::plugin_rows(&f)?;
    let statuses: Vec<crate::view::PluginStatusRow> = rows
        .iter()
        .filter(|r| name.as_deref().map(|n| n == r.name).unwrap_or(true))
        .map(|r| {
            let run_state = crate::plugins::read_run_state(&f.paths.home, &r.name);
            crate::view::PluginStatusRow::new(r.name.clone(), r.enabled, &run_state)
        })
        .collect();
    if let Some(n) = &name
        && statuses.is_empty()
    {
        bail!("no such plugin: {n:?}");
    }
    if json {
        if name.is_some() {
            out!("{}", serde_json::to_string_pretty(&statuses[0])?);
        } else {
            out!("{}", serde_json::to_string_pretty(&statuses)?);
        }
        return Ok(());
    }
    for s in &statuses {
        out!(
            "{:<16} {}  {}",
            s.name,
            if s.enabled { "enabled" } else { "disabled" },
            match s.state.as_str() {
                "running" => format!(
                    "running pid {}, up {}s",
                    s.pid.unwrap_or(0),
                    s.uptime_secs.unwrap_or(0)
                ),
                "restarting" => format!("restarting (x{})", s.restart_count.unwrap_or(0)),
                _ => match &s.last_exit {
                    Some(e) => format!("stopped: {e}"),
                    None => "stopped".to_string(),
                },
            }
        );
    }
    Ok(())
}

fn plugin_set_enabled(name: String, enabled: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let home_cfg = config::load_home(&f.paths.home)?;
    let cat = crate::plugins::load_catalog(&f.paths.home, &home_cfg.plugin_dirs);
    if !cat.plugins.contains_key(&name) {
        bail!("no such plugin: {name:?}");
    }
    f.store.set_plugin_enabled(&name, enabled, unix_now())?;
    out!("{name} {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

fn plugin_restart(name: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    let home_cfg = config::load_home(&f.paths.home)?;
    let cat = crate::plugins::load_catalog(&f.paths.home, &home_cfg.plugin_dirs);
    if !cat.plugins.contains_key(&name) {
        bail!("no such plugin: {name:?}");
    }
    if !f.store.enabled_plugins()?.contains(&name) {
        bail!("plugin {name:?} is not enabled");
    }
    crate::plugins::request_restart(&f.paths.home, &name)?;
    out!("{name} restart requested");
    Ok(())
}

fn plugin_install(path: PathBuf) -> Result<()> {
    let f = Forge::open(false, false)?;
    let manifest = crate::plugins::install(&f.paths.home, &path)?;
    out!(
        "installed {} at {}",
        manifest.name,
        f.paths.home.join("plugins").join(&manifest.name).display()
    );
    if let Some(build) = &manifest.build {
        out!("build {} ok", build.join(" "));
    }
    Ok(())
}

fn plugin_uninstall(name: String) -> Result<()> {
    let f = Forge::open(false, false)?;
    f.store.set_plugin_enabled(&name, false, unix_now())?;
    crate::plugins::remove_installed(&f.paths.home, &name)?;
    out!("uninstalled {name}; left plugins-state/{name} alone");
    Ok(())
}

fn plugin_logs(name: String, follow: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let path = f
        .paths
        .home
        .join("logs")
        .join("plugins")
        .join(format!("{name}.log"));
    use std::io::Read;
    let mut file = std::fs::File::open(&path)
        .with_context(|| format!("no log yet for plugin {name:?} ({})", path.display()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    std::io::stdout().write_all(&buf)?;
    std::io::stdout().flush()?;
    if follow {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let mut more = Vec::new();
            file.read_to_end(&mut more)?;
            if !more.is_empty() {
                std::io::stdout().write_all(&more)?;
                std::io::stdout().flush()?;
            }
        }
    }
    Ok(())
}

async fn dispatch_plugin(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Plugin { cmd } => match cmd {
            PluginCmd::List { json } => plugin_list(json),
            PluginCmd::Status { name, json } => plugin_status(name, json),
            PluginCmd::Enable { name } => plugin_set_enabled(name, true),
            PluginCmd::Disable { name } => plugin_set_enabled(name, false),
            PluginCmd::Restart { name } => plugin_restart(name),
            PluginCmd::Install { path } => plugin_install(path),
            PluginCmd::Uninstall { name } => plugin_uninstall(name),
            PluginCmd::Logs { name, follow } => plugin_logs(name, follow),
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_deploy(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Deploy(a) => match a.cmd {
            Some(DeploySub::Log {
                project,
                name,
                json,
            }) => deploy_log(project, name, json),
            None => deploy_run(a.project, a.name, a.sha, a.force).await,
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_provision(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Provision(a) => provision_run(a.project, a.name, a.args).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Plugin { .. } => dispatch_plugin(cmd).await,
        Cmd::Deploy(..) => dispatch_deploy(cmd).await,
        Cmd::Provision(..) => dispatch_provision(cmd).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}
