use super::*;
use crate::workflows;

#[derive(Subcommand)]
pub(super) enum WorkflowsCmd {
    /// Load every `.forge/workflows/*.toml` and `.forge/workflows/actions/*.toml`
    /// under a path (default: the current directory) with the catalog's own
    /// parser, and report every problem with file, line, and message. No
    /// store, no FORGE_HOME: a repository's own check, run wherever the
    /// `forge` binary is (see docs/WORKFLOWS.md)
    Validate {
        /// Directory to check (default: the current directory)
        path: Option<PathBuf>,
    },
    /// Print one workflow in full: its file text, where it came from, kind,
    /// every resolved step (the action's name, kind, contract, model,
    /// turns, timeout, description), and its measured profile
    Show {
        name: String,
        /// Also look in this project's own repository workflows
        /// (`.forge/workflows/`, at its latest landed commit) if the
        /// operator catalog has no workflow of this name
        #[arg(long)]
        project: Option<String>,
        /// Machine-readable
        #[arg(long)]
        json: bool,
    },
    /// Validate a candidate workflow file's text against the catalog
    /// without writing anything: every problem with line and message, so
    /// an editor can check as the operator types
    Lint {
        /// Read the candidate file's text from stdin (the only source
        /// today)
        #[arg(long)]
        stdin: bool,
        /// The file name the candidate would be saved under; defaults to
        /// its own declared `name`
        #[arg(long)]
        name: Option<String>,
    },
    /// Write a candidate workflow file into the operator's catalog once
    /// it lints clean, commit it in the catalog's own git, and print the
    /// new commit hash; or, with `--repo`, file a direct task on that
    /// repository's project that lands the same content through review
    /// instead of writing it directly (docs/CLIENT.md, "write verb")
    Put {
        /// The file name (without `.toml`) to save the candidate under;
        /// must match its own declared `name`
        name: String,
        /// Read the candidate file's text from stdin (the only source
        /// today)
        #[arg(long)]
        stdin: bool,
        /// Commit message in the catalog's git; must not be empty
        #[arg(long)]
        message: String,
        /// File a direct task on this repository's project instead of
        /// writing to the operator's catalog
        #[arg(long)]
        repo: Option<PathBuf>,
    },
}

fn measure(f: &Forge, w: &workflows::Workflow) -> Result<profile::Measured> {
    profile::measure(&f.store, &w.name, &w.hash)
}

fn validate_workflows(path: Option<PathBuf>) -> Result<()> {
    let root = path.unwrap_or_else(|| PathBuf::from("."));
    let report = workflows::validate_repo(&root)?;
    if report.problems.is_empty() {
        out!(
            "{} workflow(s), {} action(s) valid",
            report.workflows,
            report.actions
        );
        return Ok(());
    }
    for p in &report.problems {
        match p.line {
            Some(line) => out!("{}:{}: {}", p.file.display(), line, p.message),
            None => out!("{}: {}", p.file.display(), p.message),
        }
    }
    std::process::exit(1);
}

/// The `"measured"` object `forge workflows --json` and `forge workflows
/// show --json` both print for one workflow: current version, previous
/// version if any (with regression flag), all versions combined, and the
/// same breakdown per provider.
/// A workflow version's directive share of cost (docs/EXECUTION.md, rule
/// 4): `None` when it has cost nothing.
fn directive_share(f: &Forge, w: &workflows::Workflow) -> Option<f64> {
    let costs = f.store.directive_costs(&Default::default()).ok()?;
    let (directive, total) = costs.get(&(w.name.clone(), w.hash.clone()))?;
    (*total > 0.0).then(|| directive / total)
}

fn measured_doc(f: &Forge, w: &workflows::Workflow) -> Result<serde_json::Value> {
    let m = measure(f, w)?;
    Ok(serde_json::json!({
        "current": m.current, "previous": m.previous.as_ref().map(|(h, p)| serde_json::json!({"hash": h, "profile": p})),
        "all_versions": m.all, "regressed": m.regressed,
        "directive_share": directive_share(f, w),
        "by_provider": profile::measure_by_provider(&f.store, &w.name, &w.hash).ok().map(|ps| ps.into_iter().map(|(provider, pm)| serde_json::json!({
            "provider": provider, "current": pm.current,
            "previous": pm.previous.as_ref().map(|(h, p)| serde_json::json!({"hash": h, "profile": p})),
            "regressed": pm.regressed,
        })).collect::<Vec<_>>()),
    }))
}

fn lint_workflow(stdin: bool, name: Option<String>) -> Result<()> {
    anyhow::ensure!(
        stdin,
        "forge workflows lint needs --stdin; that is the only source of the candidate text today"
    );
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the candidate workflow's text from stdin")?;
    let paths = crate::ctx::Paths::resolve()?;
    let problems = workflows::lint(&paths.home, name.as_deref(), &text)?;
    out!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "problems": problems.iter().map(|p| serde_json::json!({"line": p.line, "message": p.message})).collect::<Vec<_>>(),
        }))?
    );
    if !problems.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// `forge workflows put NAME --stdin --message TEXT [--repo PATH]`: see
/// docs/CLIENT.md, "write verb". Reads the candidate from stdin and
/// lints it against the operator's catalog exactly as `forge workflows
/// lint` does, refusing (writing nothing) on any lint problem, a NAME
/// that doesn't match the candidate's own declared `name`, or an empty
/// `--message`. With no `--repo`, writes the file into the catalog and
/// commits just that file in the catalog's own git (already a
/// repository; `commit_path` leaves any other dirty file in there
/// untouched), printing the new commit hash. With `--repo PATH`, files a
/// direct task on that repository's project instead of touching the
/// catalog: the task adds or replaces `.forge/workflows/NAME.toml` with
/// the candidate's exact content, so a repository's own automation still
/// lands through the normal build-and-verify path rather than a direct
/// write; prints the new task's id.
async fn put_workflow(
    name: String,
    stdin: bool,
    message: String,
    repo: Option<PathBuf>,
) -> Result<()> {
    anyhow::ensure!(
        stdin,
        "forge workflows put needs --stdin; that is the only source of the candidate text today"
    );
    anyhow::ensure!(!message.trim().is_empty(), "--message must not be empty");
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the candidate workflow's text from stdin")?;
    let paths = crate::ctx::Paths::resolve()?;
    let problems = workflows::lint(&paths.home, Some(&name), &text)?;
    if !problems.is_empty() {
        for p in &problems {
            match p.line {
                Some(line) => out!("{name}.toml:{line}: {}", p.message),
                None => out!("{name}.toml: {}", p.message),
            }
        }
        bail!("{name} fails lint; nothing written");
    }
    let declared = workflows::declared_name(&text);
    anyhow::ensure!(
        declared.as_deref() == Some(name.as_str()),
        "{name} does not match the candidate's own declared name ({declared:?}); refusing to write"
    );

    match repo {
        Some(repo) => {
            let f = Forge::open(false, false)?;
            let req = crate::queue::TaskRequest {
                repo,
                task: format!(
                    "Add or replace the file `.forge/workflows/{name}.toml` in this repository with exactly this content, byte for byte (create it if it doesn't exist, overwrite it if it does):\n\n```toml\n{text}\n```"
                ),
                max_turns: 100,
                retries: 1,
                timeout_secs: 1800,
                ..Default::default()
            };
            let t = crate::queue::enqueue(&f, &req, None).await?;
            out!("{}", t.id);
        }
        None => {
            let dir = workflows::catalog_dir(&paths.home)?;
            let file = format!("{name}.toml");
            std::fs::write(dir.join(&file), &text)
                .with_context(|| format!("writing {file} into the catalog"))?;
            let hash = match git::commit_path(&dir, &file, &message).await? {
                Some(h) => h,
                None => git::rev_parse(&dir, "HEAD").await?,
            };
            out!("{hash}");
        }
    }
    Ok(())
}

async fn show_workflow(name: String, project: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let mut found: Option<(workflows::Workflow, &'static str)> =
        workflows::get(&f.paths.home, &name)?.map(|w| (w, "catalog"));
    let mut repo_ctx: Option<(PathBuf, String)> = None;
    if let Some(p) = &project {
        f.store
            .project(p)?
            .with_context(|| format!("no project {p}"))?;
        let repo = f
            .store
            .first_repo(p)?
            .with_context(|| format!("project {p} has no registered repository"))?;
        let repo_path = PathBuf::from(&repo);
        let cfg = config::load_working(&repo_path).await?;
        let landed_sha = git::rev_parse(&repo_path, &format!("refs/heads/{}", cfg.base_branch))
            .await
            .with_context(|| format!("resolving {} on {}", cfg.base_branch, repo_path.display()))?;
        if found.is_none() {
            let repo_workflows = workflows::load_all_at(&repo_path, &landed_sha)?;
            found = repo_workflows
                .into_iter()
                .find(|w| w.name == name)
                .map(|w| (w, "repo"));
        }
        repo_ctx = Some((repo_path, landed_sha));
    }
    let (wf, source) = found.with_context(|| {
        format!("unknown workflow {name:?}; see `forge workflows` for what is configured")
    })?;

    let step_doc = |action: &workflows::ActionDef,
                    model: Option<String>,
                    max_turns: Option<u32>,
                    timeout_secs: Option<u32>| {
        serde_json::json!({
            "name": action.name, "kind": action.kind, "contract": action.contract,
            "model": model, "max_turns": max_turns, "timeout_secs": timeout_secs,
            "description": action.description,
        })
    };
    let steps: Vec<serde_json::Value> = if wf.kind == workflows::WorkflowKind::Run {
        let run_steps = if source == "repo" {
            let (repo_path, landed_sha) = repo_ctx.as_ref().unwrap();
            workflows::resolve_job_at(&f.paths.home, repo_path, landed_sha, &name)?
                .context("the workflow disappeared from the repository while resolving it")?
                .1
        } else {
            workflows::resolve_job(&f.paths.home, &name)?.1
        };
        run_steps
            .iter()
            .map(|s| step_doc(&s.action, s.model.clone(), s.max_turns, s.timeout_secs))
            .collect()
    } else if source == "catalog" {
        workflows::resolve(&f.paths.home, &name)
            .with_context(|| format!("resolving workflow {name:?}"))?
            .steps
            .iter()
            .map(|s| step_doc(&s.action, s.model.clone(), s.max_turns, s.timeout_secs))
            .collect()
    } else {
        Vec::new()
    };
    let measured = measured_doc(&f, &wf)?;

    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": wf.name, "source": source, "path": wf.path, "kind": wf.kind,
                "text": wf.text, "steps": steps, "measured": measured,
            }))?
        );
        return Ok(());
    }

    out!(
        "{} [{}] {} ({})",
        wf.name,
        wf.kind,
        source,
        wf.path.display()
    );
    out!("{}", wf.description);
    for s in &steps {
        out!(
            "  {:<12} {:<10} model={:<10} turns={:<4} timeout={:<6} {}",
            s["name"].as_str().unwrap_or_default(),
            s["contract"].as_str().unwrap_or_default(),
            s["model"].as_str().unwrap_or("-"),
            s["max_turns"]
                .as_u64()
                .map_or("-".into(), |n| n.to_string()),
            s["timeout_secs"]
                .as_u64()
                .map_or("-".into(), |n| n.to_string()),
            s["description"].as_str().unwrap_or_default(),
        );
    }
    let m = measure(&f, &wf)?;
    out!(
        "measured   {}{}",
        m.current.line(),
        if m.regressed { "  REGRESSION" } else { "" }
    );
    out!();
    out!("{}", wf.text);
    Ok(())
}

async fn list_workflows(project: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let all = workflows::load_all(&f.paths.home)?;
    let actions = workflows::load_actions(&f.paths.home)?;
    let direct = all
        .iter()
        .find(|w| w.name == "direct")
        .map(|w| measure(&f, w))
        .transpose()?;
    let repo_workflows: Vec<workflows::Workflow> = match &project {
        Some(p) => {
            f.store
                .project(p)?
                .with_context(|| format!("no project {p}"))?;
            let repo = f
                .store
                .first_repo(p)?
                .with_context(|| format!("project {p} has no registered repository"))?;
            let repo_path = PathBuf::from(&repo);
            let cfg = config::load_working(&repo_path).await?;
            let landed_sha = git::rev_parse(&repo_path, &format!("refs/heads/{}", cfg.base_branch))
                .await
                .with_context(|| {
                    format!("resolving {} on {}", cfg.base_branch, repo_path.display())
                })?;
            workflows::load_all_at(&repo_path, &landed_sha)?
        }
        None => Vec::new(),
    };
    if json {
        let doc = |w: &workflows::Workflow, source: &str| {
            let is_run = w.kind == workflows::WorkflowKind::Run;
            let m = measure(&f, w).ok();
            let resolved = (!is_run && source == "catalog")
                .then(|| workflows::resolve(&f.paths.home, &w.name).ok())
                .flatten();
            serde_json::json!({
                "name": w.name, "source": source, "kind": w.kind, "hash": w.hash, "description": w.description, "path": w.path,
                "steps": w.steps,
                "trigger": w.trigger, "assert": w.assert, "limits": w.limits,
                "resolved": resolved.as_ref().map(|r| r.steps.iter().map(|s| serde_json::json!({"action": s.action.name, "kind": s.action.kind, "contract": s.action.contract, "hash": s.action.hash, "via": s.via, "model": s.model, "max_turns": s.max_turns, "timeout_secs": s.timeout_secs})).collect::<Vec<_>>()),
                "meta": w.meta,
                "measured": m.as_ref().map(|m| serde_json::json!({
                    "current": m.current, "previous": m.previous.as_ref().map(|(h, p)| serde_json::json!({"hash": h, "profile": p})),
                    "all_versions": m.all, "regressed": m.regressed,
                    "directive_share": directive_share(&f, w),
                    "by_provider": profile::measure_by_provider(&f.store, &w.name, &w.hash).ok().map(|ps| ps.into_iter().map(|(provider, pm)| serde_json::json!({
                        "provider": provider, "current": pm.current,
                        "previous": pm.previous.as_ref().map(|(h, p)| serde_json::json!({"hash": h, "profile": p})),
                        "regressed": pm.regressed,
                    })).collect::<Vec<_>>()),
                    "cost_vs_direct": match (&direct, m.current.known) {
                        (Some(d), true) if d.current.known && d.current.cost_per_task > 0.0 => Some(m.current.cost_per_task / d.current.cost_per_task),
                        _ => None,
                    },
                })),
            })
        };
        let mut docs: Vec<serde_json::Value> = all.iter().map(|w| doc(w, "catalog")).collect();
        docs.extend(repo_workflows.iter().map(|w| doc(w, "repo")));
        let acts: Vec<serde_json::Value> = actions
            .values()
            .map(|a| serde_json::json!({"name": a.name, "kind": a.kind, "contract": a.contract, "hash": a.hash, "description": a.description, "consumes": a.consumes, "produces": a.produces, "run": a.run, "check": a.check, "paths": a.paths, "brief": a.brief, "max_turns": a.max_turns, "timeout_secs": a.timeout_secs, "model": a.model}))
            .collect();
        out!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"workflows": docs, "actions": acts, "min_runs_for_known": profile::MIN_N, "lookback": LOOKBACK})
            )?
        );
        return Ok(());
    }
    for (w, source) in all
        .iter()
        .map(|w| (w, "catalog"))
        .chain(repo_workflows.iter().map(|w| (w, "repo")))
    {
        let tag = match (w.kind == workflows::WorkflowKind::Run, source) {
            (true, "repo") => " [run/repo]",
            (true, _) => " [run]",
            (false, "repo") => " [repo]",
            (false, _) => "",
        };
        out!(
            "{:<12}{} {}  {:<24} {}",
            w.name,
            tag,
            &w.hash[..8],
            w.steps_text(),
            w.description
        );
        if let Some(t) = &w.trigger {
            out!(
                "             trigger    {}",
                match t.value() {
                    Some(v) => format!("{} → {v}", t.on),
                    None => t.on.to_string(),
                }
            );
        }
        if w.kind == workflows::WorkflowKind::Run || source == "repo" {
            // Jobs are not yet resolved or measured; that is later build
            // order (docs/JOBS.md). A repository workflow resolves against
            // its own pinned commit, not the operator's catalog by name.
            out!("             {}", w.path.display());
            continue;
        }
        match workflows::resolve(&f.paths.home, &w.name) {
            Ok(r) => out!(
                "             resolves   {}",
                r.steps
                    .iter()
                    .map(|s| format!("{}@{}", s.action.name, &s.action.hash[..8]))
                    .collect::<Vec<_>>()
                    .join(" → ")
            ),
            Err(e) => out!("             BROKEN     {e:#}"),
        }
        out!("             use when   {}", w.meta.use_when);
        out!("             avoid when {}", w.meta.avoid_when);
        if !w.meta.requires.is_empty() {
            out!("             requires   {}", w.meta.requires.join("; "));
        }
        let m = measure(&f, w)?;
        out!("             measured   {}", m.current.line());
        for (provider, pm) in profile::measure_by_provider(&f.store, &w.name, &w.hash)? {
            out!("             on {:<8}  {}", provider, pm.current.line());
        }
        if let (Some(d), true) = (&direct, m.current.known)
            && d.current.known
            && d.current.cost_per_task > 0.0
            && w.name != "direct"
        {
            out!(
                "             cost       {:.1}x direct (measured)",
                m.current.cost_per_task / d.current.cost_per_task
            );
        }
        if let Some((h, p)) = &m.previous {
            out!(
                "             previous   {}: {}{}",
                &h[..h.len().min(8)],
                p.line(),
                if m.regressed { "  REGRESSION" } else { "" }
            );
        }
        if m.all.n > m.current.n {
            out!("             all vers.  {}", m.all.line());
        }
        out!("             {}", w.path.display());
    }
    out!();
    for a in actions.values() {
        let what = match (&a.run, &a.check) {
            (Some(r), _) => format!(
                "run {}",
                r.iter()
                    .map(|a| match a.trim().split_once('\n') {
                        // A multi-line script: its first line stands for it.
                        Some((first, _)) => format!("{first} …"),
                        None => a.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            (None, Some(c)) => format!("repo check `{c}`"),
            _ => {
                if a.contract.as_str() != a.name {
                    format!("contract {}", a.contract)
                } else {
                    String::new()
                }
            }
        };
        let flow = match (a.consumes.is_empty(), a.produces.is_empty()) {
            (true, true) => String::new(),
            _ => format!(
                "  {} → {}",
                a.consumes
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                a.produces
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        };
        out!(
            "{:<12} {}  {:<10} {}{}{}",
            a.name,
            &a.hash[..8],
            format!("{:?}", a.kind).to_lowercase(),
            a.description,
            if what.is_empty() {
                String::new()
            } else {
                format!("  [{what}]")
            },
            flow
        );
        if let Some(c) = workflows::commit_for(&f.paths.home, &a.hash) {
            out!("             since      {c}");
        }
    }
    Ok(())
}

fn list_providers(json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    if json {
        let docs: Vec<serde_json::Value> = f
            .providers
            .values()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "runner": p.runner.as_str(),
                    "model": p.model,
                    "base_url": p.base_url,
                    "api_key_env": p.api_key_env,
                    "env": p.env.iter().map(|(k, _)| k).collect::<Vec<_>>(),
                    "extra_args": p.extra_args,
                    "notes": p.notes,
                })
            })
            .collect();
        out!("{}", serde_json::to_string_pretty(&docs)?);
        return Ok(());
    }
    for p in f.providers.values() {
        out!(
            "{:<12} {:<10} {}",
            p.name,
            p.runner.as_str(),
            p.model.as_deref().unwrap_or("(runner default)"),
        );
        if let Some(n) = &p.notes {
            out!("             {n}");
        }
    }
    Ok(())
}

async fn dispatch_workflows(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Workflows { cmd, project, json } => match cmd {
            Some(WorkflowsCmd::Validate { path }) => validate_workflows(path),
            Some(WorkflowsCmd::Show {
                name,
                project,
                json,
            }) => show_workflow(name, project, json).await,
            Some(WorkflowsCmd::Lint { stdin, name }) => lint_workflow(stdin, name),
            Some(WorkflowsCmd::Put {
                name,
                stdin,
                message,
                repo,
            }) => put_workflow(name, stdin, message, repo).await,
            None => list_workflows(project, json).await,
        },
        _ => unreachable!("command routed to the wrong family"),
    }
}

async fn dispatch_providers(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Providers { json } => list_providers(json),
        _ => unreachable!("command routed to the wrong family"),
    }
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Workflows { .. } => dispatch_workflows(cmd).await,
        Cmd::Providers { .. } => dispatch_providers(cmd).await,
        _ => unreachable!("command routed to the wrong family"),
    }
}
