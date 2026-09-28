use super::gc::gc;
use super::statistics::stats;
use super::web::*;
use super::*;

#[derive(Subcommand)]
pub(super) enum WebCmd {
    /// Print the tokened link for `forge-web` at `--bind` (default
    /// `127.0.0.1:7788`): `http://ADDR/?token=...`. Reads
    /// `FORGE_HOME/web.token`, creating it the same way `forge-web`
    /// itself does if it is not there yet, so this works whether
    /// `forge-web` is already running or not started yet.
    Link {
        /// The address forge-web binds (or will bind)
        #[arg(long, default_value = "127.0.0.1:7788")]
        bind: String,
    },
    /// The same link as `forge web link`, handed to `xdg-open`
    Open {
        /// The address forge-web binds (or will bind)
        #[arg(long, default_value = "127.0.0.1:7788")]
        bind: String,
    },
    /// Run `forge-web`, found beside the `forge` binary (else on PATH),
    /// passing `--bind` through
    Serve {
        /// The address forge-web binds
        #[arg(long)]
        bind: Option<String>,
    },
}

fn version() -> Result<()> {
    let sha = env!("FORGE_GIT_SHA");
    if sha.is_empty() {
        out!("{}", env!("CARGO_PKG_VERSION"));
    } else {
        out!("{} ({})", env!("CARGO_PKG_VERSION"), sha);
    }
    Ok(())
}

/// `forge init [--home DIR]`: see `Cmd::Init`.
async fn cmd_init(home: Option<PathBuf>, relink: bool, mirror: Option<String>) -> Result<()> {
    let report = crate::init::run(home, relink, mirror.as_deref()).await?;
    for s in &report.steps {
        let tag = if s.changed { "done" } else { "ok  " };
        out!("{tag} {:<10} {}", s.name, s.detail);
    }
    if !report.changed_anything() {
        out!("already initialized; nothing changed");
    }
    out!();
    let paths = crate::ctx::Paths::for_home(report.home)?;
    let checks = doctor::run_at(paths)?;
    if print_doctor_checks(&checks) {
        std::process::exit(1);
    }
    Ok(())
}

/// Prints each check as `forge doctor`'s own text format does, and
/// answers whether any of them failed. Shared with `forge init`, whose
/// closing pass is exactly this against the home it just set up.
fn print_doctor_checks(checks: &[doctor::Check]) -> bool {
    let failed = checks.iter().any(|c| c.status == doctor::Status::Fail);
    for c in checks {
        let tag = match c.status {
            doctor::Status::Ok => "OK  ",
            doctor::Status::Warn => "WARN",
            doctor::Status::Fail => "FAIL",
        };
        out!("{tag} {:<12} {}", c.name, c.detail);
        if !c.hint.is_empty() && c.status != doctor::Status::Ok {
            out!("     {:<12} → {}", "", c.hint);
        }
    }
    failed
}

fn run_doctor(json: bool, only: Vec<String>) -> Result<()> {
    let checks = if only.is_empty() {
        doctor::run()?
    } else {
        doctor::run_only(&only)?
    };
    // With --only, a named check that did not run is as bad as one that
    // failed: a caller gating on it must not pass on its absence.
    let missing = only
        .iter()
        .any(|name| !checks.iter().any(|c| &c.name == name));
    if json {
        let failed = missing || checks.iter().any(|c| c.status == doctor::Status::Fail);
        out!("{}", serde_json::to_string(&checks)?);
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }
    if print_doctor_checks(&checks) || missing {
        std::process::exit(1);
    }
    Ok(())
}

fn graph(repo: PathBuf, json: bool) -> Result<()> {
    let bin = crate::graph::repomap_bin()?;
    let mut g = crate::graph::build(&repo, &bin)?;
    if json {
        // The overlay is additive: a store this repository never queued
        // a task under, or no store at all, just leaves every node's
        // overlay at its empty default.
        if let (Ok(f), Ok(repo)) = (Forge::open(false, false), repo.canonicalize()) {
            crate::graph::overlay(&f.store, &repo.display().to_string(), &mut g)?;
        }
        out!("{}", serde_json::to_string_pretty(&g)?);
        return Ok(());
    }
    let files = g.nodes.iter().filter(|n| n.kind == "file").count();
    let modules = g.nodes.iter().filter(|n| n.kind == "module").count();
    out!(
        "{files} file(s), {modules} module(s), {} edge(s)",
        g.edges.len()
    );
    Ok(())
}

fn requests(repo: Option<PathBuf>, grep: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let repo = repo
        .map(|p| p.canonicalize().context("repo path"))
        .transpose()?
        .map(|p| p.display().to_string());
    let rows = requests_json(&f, repo.as_deref(), grep.as_deref())?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("no blocked tasks");
        return Ok(());
    }
    out!(
        "{:<5} {:<9} {:<8} {:<18} REQUEST",
        "ID",
        "KIND",
        "WF",
        "REPO"
    );
    for r in &rows {
        let repo_name = Path::new(&r.repo)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out!(
            "{:<5} {:<9} {:<8} {:<18} {}",
            r.id,
            r.kind,
            r.workflow,
            repo_name,
            r.text
        );
        if !r.tried.is_empty() {
            out!("{:<44} did: {}", "", r.tried);
        }
    }
    Ok(())
}

fn snapshot() -> Result<()> {
    let f = Forge::open(false, false)?;
    let offset = crate::report::log::snapshot(&f.paths.home.join("events.jsonl"))?.to_string();
    let doc = serde_json::json!({
        "tasks": tasks_json(&f, &crate::store::TaskFilter { limit: 200, ..Default::default() })?,
        "requests": requests_json(&f, None, None)?,
        "worker": worker_json(&f),
        "events_offset": offset,
    });
    out!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}

fn events(since: Option<String>, follow: bool, task: Option<i64>) -> Result<()> {
    use crate::report::log::{self, Cursor};
    use std::io::Write;
    let paths = crate::ctx::Paths::resolve()?;
    let path = paths.home.join("events.jsonl");
    // A follower with no `--since` is a named subscription: the kernel keeps
    // its place, so a restart resumes after the last line it was handed.
    // An explicit `--since` is the caller's own cursor and is left alone.
    let subscription = if follow && since.is_none() {
        let name = std::env::var("FORGE_PLUGIN_NAME")
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "default".to_string());
        let store = crate::store::Store::open(&paths.home.join("forge.db"))?;
        Some((name, store))
    } else {
        None
    };
    let mut pos: Cursor = match (&since, &subscription) {
        (Some(s), _) => s.parse()?,
        (None, Some((name, store))) => store
            .subscription_cursor(name)?
            .as_deref()
            .unwrap_or("0")
            .parse()?,
        (None, None) => "0".parse()?,
    };
    let mut stdout = std::io::stdout().lock();
    loop {
        let batch = log::read(&path, pos, 8 * 1024 * 1024)?;
        if batch.resync && writeln!(stdout, "{}", serde_json::json!({
            "type": "resync", "cursor": batch.lines.first().map(|l| l.0).unwrap_or(batch.next).to_string()
        })).is_err() { return Ok(()); }
        for (start, cursor, line) in batch.lines {
            let Ok(mut event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if !event.is_object() {
                continue;
            }
            if task.is_some_and(|id| event["task"].as_i64() != Some(id)) {
                continue;
            }
            event["cursor"] = cursor.to_string().into();
            // A line counts as delivered once it is handed to the pipe, so
            // the subscription moves past it first: a crash after this
            // never replays it. A write that fails puts the cursor back.
            if let Some((name, store)) = &subscription {
                store.set_subscription_cursor(name, &cursor.to_string())?;
            }
            if writeln!(stdout, "{event}").is_err() {
                if let Some((name, store)) = &subscription {
                    store.set_subscription_cursor(name, &start.to_string())?;
                }
                return Ok(());
            }
        }
        let _ = stdout.flush();
        if let Some((name, store)) = &subscription
            && pos != batch.next
        {
            store.set_subscription_cursor(name, &batch.next.to_string())?;
        }
        let advanced = pos != batch.next;
        pos = batch.next;
        if advanced {
            continue;
        }
        if !follow {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

async fn dispatch_gc(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Gc {
            dry_run,
            older_than,
        } => gc(dry_run, older_than).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_init(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Init {
            home,
            relink,
            mirror,
        } => cmd_init(home, relink, mirror).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_demo(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Demo { fake, reset } => super::demo::demo(fake, reset),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_doctor(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Doctor { json, only } => run_doctor(json, only),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_version(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Version => version(),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_upgrade(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Upgrade {
            source,
            check_only,
            force,
        } => crate::upgrade::run(source, check_only, force),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_egressrelay(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::EgressRelay {
            socket,
            listen,
            ready,
            refused,
        } => crate::egress::relay(&socket, &listen, ready.as_deref(), refused.as_deref()).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_graph(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Graph { repo, json } => graph(repo, json),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_requests(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Requests { repo, grep, json } => requests(repo, grep, json),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_stats(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Stats {
            tools,
            step,
            quality,
            journal,
            tests,
            last,
            by_role,
            by_step,
            factors,
            questions,
            days,
            project,
            initiative,
            reprice,
            provider,
            force,
            json,
        } => {
            stats(crate::cli::statistics::StatsOptions {
                tools,
                step,
                quality,
                journal,
                tests,
                last,
                by_role,
                by_step,
                factors,
                questions,
                days,
                project,
                initiative,
                reprice,
                provider,
                force,
                json,
            })
            .await
        }
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_events(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Events {
            since,
            follow,
            task,
        } => events(since, follow, task),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_snapshot(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Snapshot => snapshot(),
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_web(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Web { cmd } => match cmd {
            WebCmd::Link { bind } => web_link(bind),
            WebCmd::Open { bind } => web_open(bind),
            WebCmd::Serve { bind } => web_serve(bind),
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Gc { .. } => dispatch_gc(cmd).await,
        Cmd::Init { .. } => dispatch_init(cmd).await,
        Cmd::Doctor { .. } => dispatch_doctor(cmd).await,
        Cmd::Demo { .. } => dispatch_demo(cmd).await,
        Cmd::Version => dispatch_version(cmd).await,
        Cmd::Upgrade { .. } => dispatch_upgrade(cmd).await,
        Cmd::EgressRelay { .. } => dispatch_egressrelay(cmd).await,
        Cmd::Graph { .. } => dispatch_graph(cmd).await,
        Cmd::Requests { .. } => dispatch_requests(cmd).await,
        Cmd::Eval { .. } => super::eval::dispatch(cmd).await,
        Cmd::Stats { .. } => dispatch_stats(cmd).await,
        Cmd::Events { .. } => dispatch_events(cmd).await,
        Cmd::Snapshot => dispatch_snapshot(cmd).await,
        Cmd::Web { .. } => dispatch_web(cmd).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}
