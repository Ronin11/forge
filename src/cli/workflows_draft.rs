//! `forge workflows draft`: the draft editor (docs/WORKFLOWS.md, "The draft
//! editor"). With a NAME it is a session — one edit per line on stdin, the
//! draft linted with the catalog's own linter after every change — and with
//! a subcommand it is the stateless verb the web editor calls, a draft
//! document in on stdin and the annotated one out on stdout.

use super::*;
use crate::store::Initiative;
use crate::workflows::draft::{self, Annotated, Draft, DraftStep, Filed, Placeholder};
use crate::workflows::{self, Kind, WorkflowKind};

mod session;
use session::Next;

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(super) struct DraftArgs {
    #[command(subcommand)]
    cmd: Option<DraftCmd>,
    /// Edit this draft, or this catalog workflow, one command per line on
    /// stdin (`help` lists them); a name neither has starts a new draft
    name: Option<String>,
    /// A new draft's kind: build or run
    #[arg(long, default_value = "build")]
    kind: String,
    /// The project a placeholder's build task is filed on
    #[arg(long)]
    project: Option<String>,
}

#[derive(Subcommand)]
pub(super) enum DraftCmd {
    /// Saved drafts, with their status and the tasks filed for their
    /// placeholders
    List {
        #[arg(long)]
        json: bool,
    },
    /// One saved draft, or a catalog workflow as a draft, linted
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Lint the draft document on stdin (JSON); print it back annotated
    /// with the workflow file it renders to, every problem, and each
    /// step's contract. Writes nothing
    Check,
    /// Turn a workflow file's text on stdin into an annotated draft
    Import,
    /// Save the draft document on stdin under the drafts directory,
    /// `incomplete` while a placeholder's action is missing
    Save,
    /// Commit the draft document on stdin: to the catalog, or with `--repo`
    /// as a repository task; a draft with placeholders is saved incomplete
    /// and files one build task per placeholder instead
    Put {
        name: String,
        /// Commit message in the catalog's git
        #[arg(long)]
        message: String,
        /// File a task on this repository instead of writing the catalog
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// Every action a step may name, with its kind, contract and
    /// description
    Actions {
        #[arg(long)]
        json: bool,
    },
    /// Enable every incomplete draft whose placeholders' actions have
    /// landed and that lints clean (the worker does this every pass)
    Reconcile,
}

fn home() -> Result<PathBuf> {
    Ok(crate::ctx::Paths::resolve()?.home)
}

fn read_doc() -> Result<Draft> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the draft document from stdin")?;
    let d: Draft = serde_json::from_str(&text).context("the draft document is not valid JSON")?;
    anyhow::ensure!(
        draft::valid_name(&d.name),
        "{:?} is not a valid draft name",
        d.name
    );
    Ok(d)
}

/// The tasks already filed live with the saved draft, not with the
/// document a client sends back: keep the saved ones, so a second `put`
/// never files a placeholder's task twice.
fn adopt_filed(home: &Path, d: &mut Draft) -> Result<()> {
    if let Some(saved) = draft::load(home, &d.name)? {
        for t in saved.tasks {
            if !d.tasks.iter().any(|x| x.action == t.action) {
                d.tasks.push(t);
            }
        }
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    out!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// The draft named `name`: its saved draft, else the catalog's workflow of
/// that name as a draft, else a new one.
fn open_draft(home: &Path, name: &str, kind: WorkflowKind) -> Result<Draft> {
    if let Some(d) = draft::load(home, name)? {
        return Ok(d);
    }
    match workflows::get(home, name)? {
        Some(wf) => Draft::from_workflow(&wf),
        None => {
            anyhow::ensure!(
                draft::valid_name(name),
                "{name:?} is not a valid draft name"
            );
            Ok(Draft::new(name, kind))
        }
    }
}

/// The draft as text, for a terminal: its steps numbered with their
/// contracts, then every problem or `clean`.
fn show_text(a: &Annotated) -> String {
    let d = &a.draft;
    let mut s = format!(
        "{} [{}, {}]{}\n",
        d.name,
        d.kind,
        d.status.as_str(),
        if d.description.is_empty() {
            String::new()
        } else {
            format!(" {}", d.description)
        }
    );
    for (i, (st, info)) in d.steps.iter().zip(&a.info).enumerate() {
        let what = match (&st.placeholder, info.kind) {
            (Some(_), _) if info.landed => "landed".to_string(),
            (Some(p), _) => format!("PLACEHOLDER {}", p.line()),
            (None, Some(k)) => format!(
                "{} {}",
                kind_name(k),
                info.contract.as_deref().unwrap_or("")
            ),
            (None, None) => "unknown".to_string(),
        };
        s.push_str(&format!("  {}. {:<24} {what}\n", i + 1, st.action));
        for (k, to) in &st.on {
            s.push_str(&format!("       on {k} -> {to}\n"));
        }
    }
    for (i, st) in d.proposal.iter().enumerate() {
        s.push_str(&format!("  proposed {}. {}\n", i + 1, st.action));
    }
    if a.clean {
        s.push_str("lint: clean\n");
    } else {
        s.push_str(&format!("lint: {} problem(s)\n", a.problems.len()));
        for p in &a.problems {
            match p.step {
                Some(n) => s.push_str(&format!("  step {}: {}\n", n + 1, p.message)),
                None => s.push_str(&format!("  {}\n", p.message)),
            }
        }
    }
    s
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Operation => "operation",
        Kind::Directive => "directive",
    }
}

fn actions_doc(home: &Path) -> Result<Vec<serde_json::Value>> {
    Ok(workflows::lint::known_actions(home)?
        .values()
        .map(|a| {
            serde_json::json!({
                "name": a.name, "kind": a.kind, "contract": a.contract,
                "description": a.description, "consumes": a.consumes, "produces": a.produces,
                "outcomes": a.outcomes,
            })
        })
        .collect())
}

/// The build task a placeholder's action is filed as.
fn build_task_text(workflow: &str, action: &str, p: &Placeholder) -> String {
    format!(
        "build action {action} with contract {}\n\nThe draft workflow `{workflow}` names `{action}` in a step and cannot be enabled until the action exists. Write it as the {} action file `{action}.toml` in the catalog's `actions/`, following the existing actions (docs/ACTIONS.md); when it is there and `{workflow}` lints clean, the draft enables itself.",
        p.line(),
        kind_name(p.kind)
    )
}

/// The initiative a workflow's build tasks are filed under, `workflow
/// <name>`: an open one of that outcome, else a new one.
fn workflow_initiative(f: &Forge, project: &str, workflow: &str) -> Result<i64> {
    let outcome = format!("workflow {workflow}");
    let open = f
        .store
        .list_initiatives(Some(project))?
        .into_iter()
        .find(|i| i.outcome == outcome && i.settled_at.is_none());
    if let Some(i) = open {
        return Ok(i.id);
    }
    f.store.create_initiative(&Initiative {
        project: project.to_string(),
        outcome,
        stop_after_same_rule: 3,
        created_at: unix_now(),
        ..Default::default()
    })
}

/// File one build task per pending placeholder that has none yet, on the
/// draft's project under the initiative `workflow <name>`.
async fn file_build_tasks(
    f: &Forge,
    d: &Draft,
    pending: &[(String, Placeholder)],
) -> Result<Vec<Filed>> {
    let project = d.project.as_deref().with_context(|| {
        format!(
            "{} has placeholders, so its build tasks need a project: set one (`project NAME`, or `--project`)",
            d.name
        )
    })?;
    f.store
        .project(project)?
        .with_context(|| format!("no project {project}"))?;
    let repo = f
        .store
        .first_repo(project)?
        .with_context(|| format!("project {project} has no registered repository"))?;
    let initiative = workflow_initiative(f, project, &d.name)?;
    let mut filed = Vec::new();
    for (action, p) in pending {
        if d.tasks.iter().any(|t| &t.action == action) {
            continue;
        }
        let req = crate::queue::TaskRequest {
            repo: PathBuf::from(&repo),
            task: build_task_text(&d.name, action, p),
            max_turns: 100,
            retries: 1,
            timeout_secs: 1800,
            project: Some(project.to_string()),
            initiative: Some(initiative),
            ..Default::default()
        };
        let t = crate::queue::enqueue(f, &req, None).await?;
        filed.push(Filed {
            action: action.clone(),
            task_id: t.id,
        });
    }
    Ok(filed)
}

/// `put` for a draft. A clean draft with nothing missing is committed the
/// way `forge workflows put` commits one; a draft with placeholders is
/// saved `incomplete` and files its build tasks. Refuses, writing nothing,
/// on any lint problem. The result is what a client shows.
async fn put_draft(
    mut d: Draft,
    message: &str,
    repo: Option<PathBuf>,
) -> Result<serde_json::Value> {
    anyhow::ensure!(!message.trim().is_empty(), "--message must not be empty");
    let home = home()?;
    adopt_filed(&home, &mut d)?;
    let a = d.check(&home)?;
    if !a.clean {
        for p in &a.problems {
            out!("{}.toml: {}", d.name, p.message);
        }
        bail!("{} fails lint; nothing written", d.name);
    }
    if a.pending.is_empty() {
        let result = match repo {
            Some(repo) => {
                let f = Forge::open(false, false)?;
                let t = super::workflows::file_workflow_task(&f, repo, &d.name, &a.toml).await?;
                serde_json::json!({"result": "filed", "task_id": t})
            }
            None => {
                let hash = draft::commit_to_catalog(&home, &d.name, &a.toml, message).await?;
                serde_json::json!({"result": "committed", "hash": hash})
            }
        };
        draft::remove(&home, &d.name)?;
        return Ok(result);
    }
    anyhow::ensure!(
        repo.is_none(),
        "{} has placeholders ({}); it is put to a repository once they have landed",
        d.name,
        a.pending.join(", ")
    );
    let known: std::collections::BTreeSet<String> =
        workflows::lint::known_actions(&home)?.into_keys().collect();
    let f = Forge::open(false, false)?;
    let filed = file_build_tasks(&f, &d, &d.pending(&known)).await?;
    d.tasks.extend(filed.iter().cloned());
    d.status = draft::saved_status(d.status, a.pending.len());
    draft::save(&home, &d)?;
    Ok(serde_json::json!({
        "result": "incomplete", "status": d.status, "tasks": d.tasks, "filed": filed,
        "pending": a.pending,
    }))
}

/// `suggest`: one call to the author directive (`forge job start forge
/// author-workflow --now`), its draft turned into proposed steps. The
/// directive is a helper here, never the path a draft has to take.
fn suggest(description: &str) -> Result<(Vec<DraftStep>, String)> {
    anyhow::ensure!(
        !description.trim().is_empty(),
        "suggest needs a description"
    );
    let exe = std::env::current_exe().context("finding the forge binary")?;
    let input = std::env::temp_dir().join(format!("forge-suggest-{}.json", std::process::id()));
    std::fs::write(
        &input,
        serde_json::json!({"description": description}).to_string(),
    )?;
    let run = |args: &[&str]| -> Result<String> {
        let o = std::process::Command::new(&exe).args(args).output()?;
        anyhow::ensure!(
            o.status.success(),
            "{}",
            String::from_utf8_lossy(&o.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let started = run(&[
        "job",
        "start",
        "forge",
        "author-workflow",
        "--now",
        "--input",
        &input.to_string_lossy(),
    ]);
    let _ = std::fs::remove_file(&input);
    let started = started?;
    let id = started
        .split_whitespace()
        .next()
        .context("no job id")?
        .to_string();
    let job: serde_json::Value = serde_json::from_str(&run(&["job", "show", &id, "--json"])?)?;
    let output = job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["action"] == "draft-workflow")
        .and_then(|s| s["output_ref"].as_str())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .with_context(|| format!("job {id} left no draft; see `forge job show {id}`"))?;
    let toml = output["toml"].as_str().context("the draft has no toml")?;
    let rationale = output["rationale"].as_str().unwrap_or_default().to_string();
    Ok((Draft::from_text(toml)?.steps, rationale))
}

fn prompt(tty: bool) {
    if tty {
        print!("draft> ");
        let _ = std::io::stdout().flush();
    }
}

/// The session: read a line, edit, lint, show. A bad line reports and the
/// session goes on. Ends at `quit` or end of input.
async fn session(name: String, kind: String, project: Option<String>) -> Result<()> {
    use std::io::{BufRead, IsTerminal};
    let home = home()?;
    let kind = if kind == "run" {
        WorkflowKind::Run
    } else {
        WorkflowKind::Build
    };
    let mut d = open_draft(&home, &name, kind)?;
    d.project = project.or(d.project);
    let tty = std::io::stdin().is_terminal();
    let show = |d: &Draft| -> Result<()> {
        out!("{}", show_text(&d.check(&home)?));
        Ok(())
    };
    show(&d)?;
    prompt(tty);
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        match session::apply(&mut d, &line) {
            Err(e) => out!("error: {e:#}"),
            Ok(Next::Edited | Next::Show) => show(&d)?,
            Ok(Next::Help) => out!("{}", session::HELP),
            Ok(Next::Quit) => break,
            Ok(Next::Actions(filter)) => {
                for a in actions_doc(&home)? {
                    let n = a["name"].as_str().unwrap_or_default();
                    if filter.as_deref().is_none_or(|f| n.contains(f)) {
                        out!(
                            "{n:<28} {} {}",
                            a["kind"].as_str().unwrap_or_default(),
                            a["description"].as_str().unwrap_or_default()
                        );
                    }
                }
            }
            Ok(Next::Suggest(text)) => match suggest(&text) {
                Ok((steps, why)) => {
                    d.proposal = steps;
                    out!("{why}");
                    show(&d)?;
                }
                Err(e) => out!("error: {e:#}"),
            },
            Ok(Next::Save) => {
                let a = d.check(&home)?;
                d.status = draft::saved_status(d.status, a.pending.len());
                draft::save(&home, &d)?;
                out!("saved {} ({})", d.name, d.status.as_str());
            }
            Ok(Next::Put { message, repo }) => match put_draft(d.clone(), &message, repo).await {
                Ok(v) => out!("{v}"),
                Err(e) => out!("error: {e:#}"),
            },
        }
        prompt(tty);
    }
    Ok(())
}

pub(super) async fn dispatch(args: DraftArgs) -> Result<()> {
    let Some(cmd) = args.cmd else {
        let name = args
            .name
            .context("forge workflows draft NAME, or a subcommand (see --help)")?;
        return session(name, args.kind, args.project).await;
    };
    let home = home()?;
    match cmd {
        DraftCmd::List { json } => {
            let all = draft::list(&home)?;
            if json {
                return print_json(&all);
            }
            for d in all {
                out!(
                    "{:<28} {:<10} {} step(s), {} task(s) filed",
                    d.name,
                    d.status.as_str(),
                    d.steps.len(),
                    d.tasks.len()
                );
            }
            Ok(())
        }
        DraftCmd::Show { name, json } => {
            let d = open_draft(&home, &name, WorkflowKind::Build)?;
            let a = d.check(&home)?;
            if json {
                return print_json(&a);
            }
            out!("{}", show_text(&a));
            Ok(())
        }
        DraftCmd::Check => print_json(&read_doc()?.check(&home)?),
        DraftCmd::Import => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            print_json(&Draft::from_text(&text)?.check(&home)?)
        }
        DraftCmd::Save => {
            let mut d = read_doc()?;
            adopt_filed(&home, &mut d)?;
            let a = d.check(&home)?;
            d.status = draft::saved_status(d.status, a.pending.len());
            draft::save(&home, &d)?;
            print_json(&d.check(&home)?)
        }
        DraftCmd::Put {
            name,
            message,
            repo,
        } => {
            let d = read_doc()?;
            anyhow::ensure!(
                d.name == name,
                "{name} does not match the draft's own name {}",
                d.name
            );
            print_json(&put_draft(d, &message, repo).await?)
        }
        DraftCmd::Actions { json } => {
            let all = actions_doc(&home)?;
            if json {
                return print_json(&all);
            }
            for a in all {
                out!(
                    "{:<28} {} {}",
                    a["name"].as_str().unwrap_or_default(),
                    a["kind"].as_str().unwrap_or_default(),
                    a["description"].as_str().unwrap_or_default()
                );
            }
            Ok(())
        }
        DraftCmd::Reconcile => {
            for n in draft::reconcile(&home).await? {
                out!("enabled {n}");
            }
            Ok(())
        }
    }
}
