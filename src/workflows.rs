//! Workflows and actions are data: one TOML file each under
//! `<FORGE_HOME>/workflows/` (workflows) and `.../workflows/actions/`
//! (actions). An action is a directive (an LLM step) or an operation (a
//! deterministic step). A workflow is an ordered list of references to
//! actions or to other workflows, spliced inline.
//!
//! Identity is the git blob hash of the file. Latest by default: a task
//! resolves everything once at creation, records every hash and every
//! file's text, and runs from that record, so an edit landing mid-run
//! cannot change a running task, and reverting is git. `check` is the one
//! definition of validity; the engine refuses what it rejects.
//! See docs/ACTIONS.md and docs/WORKFLOWS.md.

use anyhow::{Context, Result, bail};
use croner::Cron;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

mod catalog;
mod contract;
mod definitions;
pub mod draft;
pub mod edges;
mod judgment;
mod library;
pub mod lint;
pub mod shadow;
mod trigger;
use catalog::{
    BUILTIN_ACTIONS, BUILTIN_OPERATIONS, blob_hash, builtin_actions_map, dir_of, ensure_sound,
    toml_files, toml_files_if_present,
};
pub(crate) use catalog::{BUILTIN_WORKFLOWS, builtin_action};
pub use catalog::{
    Catalog, catalog_dir, commit_for, declared_name, get, load_actions, load_all, load_catalog,
    uncommitted,
};
pub use contract::{Contract, Kind, OPERATION_PRODUCES, Product, WorkflowKind};
#[allow(unused_imports)] // re-exported for callers naming the field types directly
pub use definitions::{ActionDef, Meta, Pin, Resolved, ResolvedStep, StepRef, Workflow};
pub(crate) use definitions::{parse_action, parse_workflow, text_writes_hidden_tests};
pub use judgment::{OUTCOME_QUESTION, Question};
pub use library::{FRAGMENTS_DIR, Include, UNTRUSTED_DATA, text_hash};
use library::{fragment_problems, load_prompt_file};
pub use lint::{LintProblem, lint};
pub use trigger::{
    EffectKind, Limits, OnFailure, Output, Trigger, TriggerOn, default_input_bytes, parse_duration,
};
use trigger::{TriggerRaw, build_trigger};

/// Names the engine inserts itself; a user operation may not shadow them.
pub const KERNEL_OPS: &[&str] = &["verify", "push", "integrate", "land", "clone"];

fn splice(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
    path: &mut Vec<String>,
    out: &mut Resolved,
) -> Result<()> {
    if path.contains(&wf.name) {
        bail!(
            "workflow {:?} references itself through {}",
            wf.name,
            path.join(" → ")
        );
    }
    path.push(wf.name.clone());
    let pin = Pin {
        kind: "workflow".into(),
        name: wf.name.clone(),
        hash: wf.hash.clone(),
    };
    if !out.pins.contains(&pin) {
        out.pins.push(pin);
    }
    for s in &wf.steps {
        if let Some(name) = &s.workflow {
            let child = workflows.get(name).with_context(|| {
                format!(
                    "workflow {:?} references unknown workflow {:?}",
                    wf.name, name
                )
            })?;
            splice(child, workflows, actions, path, out)?;
        } else if let Some(name) = s.action.as_deref() {
            let a = actions.get(name).with_context(|| {
                format!(
                    "workflow {:?} references unknown action {:?}",
                    wf.name, name
                )
            })?;

            let pin = Pin {
                kind: "action".into(),
                name: a.name.clone(),
                hash: a.hash.clone(),
            };
            if !out.pins.contains(&pin) {
                out.pins.push(pin);
            }
            out.steps.push(ResolvedStep {
                action: a.clone(),
                model: s.model.clone().or_else(|| a.model.clone()),
                max_turns: s.max_turns.or(a.max_turns),
                timeout_secs: s.timeout_secs.or(a.timeout_secs),
                via: path.clone(),
                node: edges::node_id(out.steps.len(), &a.name),
            });
        }
    }
    path.pop();
    Ok(())
}

/// The data-flow rule: every step's `consumes` must already be produced.
/// Kernel verify after each directive produces `verdict`; the clone
/// produces `branch`.
fn check_flow(steps: &[ResolvedStep]) -> Result<()> {
    let mut have: BTreeSet<Product> = [Product::Branch].into_iter().collect();
    for (i, s) in steps.iter().enumerate() {
        for c in &s.action.consumes {
            if !have.contains(c) {
                bail!(
                    "step {} ({}) consumes {:?}, which nothing before it produces (have: {})",
                    i + 1,
                    s.action.name,
                    c.as_str(),
                    have.iter()
                        .map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        for p in &s.action.produces {
            have.insert(*p);
        }
        if s.action.kind == Kind::Directive || s.action.mutates() {
            have.insert(Product::Verdict);
            if s.action.contract == Contract::Review {
                have.insert(Product::Review);
            }
        }
    }
    if !steps.iter().any(|s| s.action.kind == Kind::Directive) {
        bail!("a workflow needs at least one directive; operations alone produce nothing to push");
    }
    for (i, s) in steps.iter().enumerate() {
        if s.action.kind == Kind::Operation
            && s.action.verifies
            && !steps[..i].iter().any(|p| p.action.kind == Kind::Directive)
        {
            bail!(
                "step {} ({}) verifies the preceding directive, but no directive precedes it",
                i + 1,
                s.action.name
            );
        }
    }
    Ok(())
}

/// Resolve a workflow by name into the exact versions a task will run,
/// splicing referenced workflows inline. Fails on unknown references,
/// cycles, and data-flow violations.
pub fn resolve(home: &Path, name: &str) -> Result<Resolved> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let wf = cat
        .workflows
        .get(name)
        .with_context(|| format!("unknown workflow {name:?}; see `forge workflows`"))?;
    let mut out = Resolved::default();
    splice(wf, &cat.workflows, &cat.actions, &mut Vec::new(), &mut out)?;
    check_flow(&out.steps)?;
    Ok(out)
}

/// A job step after resolution: the action it names, plus its own `role`
/// (a directive, routed to a provider) and any per-step overrides
/// (docs/JOBS.md, "Steps"). An operation step's `effect` is validated here
/// (see `job_steps`) but carries no further meaning to the executor: what
/// an operation actually did is read back from the effect log it writes,
/// not from what it declared.
#[derive(Clone, Debug)]
pub struct RunStep {
    pub action: ActionDef,
    pub role: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    /// The effect an operation step declares (docs/JOBS.md, "Effects").
    pub effect: Option<EffectKind>,
    /// `<index>-<action>`, stable across the run.
    pub node: String,
    /// Edges, resolved to node ids or `end`.
    pub on: BTreeMap<String, String>,
    /// How many times a loop may enter this step.
    pub max_attempts: u32,
}

/// A job step, resolved to the action it names (docs/JOBS.md, "Steps"). A
/// step may instead name a sibling `kind = "run"` workflow (`workflow =
/// "…"`, the same field a build workflow splices in); its steps are
/// inlined here, recursively, the way a build workflow's own splice works,
/// so a daily automation can be assembled from smaller run workflows
/// (docs/JOBS.md, "Steps") — `doctor-daily` splicing in `disk-and-logs` is
/// the motivating case. A cycle, or a reference to a workflow that is not
/// itself `kind = "run"`, is refused. A directive step must name a `role`
/// (routed to a provider like every role) and its action must declare a
/// `schema`; an operation step must not name a `role`, and a directive
/// step must not name an `effect`.
fn job_steps(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
) -> Result<Vec<RunStep>> {
    let mut out = Vec::new();
    job_steps_into(wf, workflows, actions, &mut Vec::new(), &mut out)?;
    edges::resolve(&wf.name, &mut out)?;
    Ok(out)
}

fn job_steps_into(
    wf: &Workflow,
    workflows: &BTreeMap<String, Workflow>,
    actions: &BTreeMap<String, ActionDef>,
    path: &mut Vec<String>,
    out: &mut Vec<RunStep>,
) -> Result<()> {
    if path.contains(&wf.name) {
        bail!(
            "run workflow {:?} references itself through {}",
            wf.name,
            path.join(" → ")
        );
    }
    path.push(wf.name.clone());
    for s in &wf.steps {
        if let Some(name) = &s.workflow {
            let child = workflows.get(name).with_context(|| {
                format!("{:?}: job step names unknown workflow {name:?}", wf.name)
            })?;
            if child.kind != WorkflowKind::Run {
                bail!(
                    "{:?}: job step names workflow {name:?}, which is kind = \"build\"; a run workflow may only splice in another run workflow",
                    wf.name
                );
            }
            job_steps_into(child, workflows, actions, path, out)?;
            continue;
        }
        let name = s.action.as_deref().with_context(|| {
            format!(
                "{:?}: a job step names exactly one of `action` or `workflow`",
                wf.name
            )
        })?;
        let action = actions
            .get(name)
            .cloned()
            .with_context(|| format!("{:?}: job step names unknown action {name:?}", wf.name))?;
        match action.kind {
            Kind::Directive => {
                if s.role.as_deref().is_none_or(|r| r.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive; it needs `role` (docs/JOBS.md, \"Steps\")",
                        wf.name
                    );
                }
                if action.schema.as_deref().is_none_or(|s| s.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive; its action {name:?} needs a `schema` (docs/JOBS.md, \"Steps\")",
                        wf.name
                    );
                }
                if s.effect.is_some() {
                    bail!(
                        "{:?}: job step {name:?} is a directive; `effect` applies to operation steps only",
                        wf.name
                    );
                }
                if s.judgment.as_deref().is_none_or(|j| j.trim().is_empty()) {
                    bail!(
                        "{:?}: job step {name:?} is a directive and carries no `judgment`; every directive step in a run workflow says what a script cannot do here: judgment = \"<one sentence>\" (docs/EXECUTION.md, rule 4: \"an operation unless judgment is genuinely needed\")",
                        wf.name
                    );
                }
            }
            Kind::Operation if s.judgment.is_some() => {
                bail!(
                    "{:?}: job step {name:?} is an operation; `judgment` applies to directive steps only",
                    wf.name
                );
            }
            Kind::Operation if s.role.is_some() => {
                bail!(
                    "{:?}: job step {name:?} is an operation; `role` applies to directive steps only",
                    wf.name
                );
            }
            Kind::Operation => {}
        }
        out.push(RunStep {
            model: s.model.clone().or_else(|| action.model.clone()),
            max_turns: s.max_turns.or(action.max_turns),
            timeout_secs: s.timeout_secs.or(action.timeout_secs),
            role: s.role.clone(),
            effect: s.effect,
            node: String::new(),
            on: s.on.clone(),
            max_attempts: s.max_attempts.unwrap_or(edges::DEFAULT_MAX_ATTEMPTS),
            action,
        });
    }
    path.pop();
    Ok(())
}

/// A run workflow by name, resolved to the exact action each of its steps
/// runs (docs/JOBS.md, "The executor"). Unlike `resolve`, there is none of
/// `check_flow`'s build-only data-flow rules: a job step's action is used
/// as written, though a step may splice in a sibling run workflow (see
/// `job_steps`). Fails on an unknown workflow, a workflow that is not
/// `kind = "run"`, an unknown action, or a step that names a workflow this
/// catalog does not have.
pub fn resolve_job(home: &Path, name: &str) -> Result<(Workflow, Vec<RunStep>)> {
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let wf = cat
        .workflows
        .get(name)
        .with_context(|| format!("unknown workflow {name:?}; see `forge workflows`"))?
        .clone();
    if wf.kind != WorkflowKind::Run {
        bail!("{name:?} is kind = \"build\"; `forge job start` runs kind = \"run\" workflows only");
    }
    let steps = job_steps(&wf, &cat.workflows, &cat.actions)?;
    Ok((wf, steps))
}

/// A run workflow read straight from a project's own repository, at
/// `.forge/workflows/<name>.toml` with its actions under
/// `.forge/workflows/actions/` (docs/JOBS.md, "Where an automation
/// lives"), rather than the operator's catalog `resolve_job` reads. Every
/// sibling `.forge/workflows/*.toml` is parsed too, so a step that splices
/// in another run workflow (`job_steps`) resolves against the repository's
/// own. `forge job bench` uses this: it measures an automation that is
/// checked into the project it belongs to, not a built-in. No `ensure`:
/// this directory is the project's own and is never git-initialised or
/// seeded with built-ins the way the operator's catalog is.
pub fn resolve_job_in_repo(repo: &Path, name: &str) -> Result<(Workflow, Vec<RunStep>)> {
    let dir = repo.join(".forge").join("workflows");
    let path = dir.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("no {} (see docs/JOBS.md)", path.display()))?;
    let wf = parse_workflow(&path, &text, blob_hash(repo, &path)?)?;
    if wf.kind != WorkflowKind::Run {
        bail!("{name:?} is kind = \"build\"; `forge job bench` runs kind = \"run\" workflows only");
    }
    let actions_dir = dir.join("actions");
    let mut actions = BTreeMap::new();
    for p in
        toml_files(&actions_dir).with_context(|| format!("reading {}", actions_dir.display()))?
    {
        let text = std::fs::read_to_string(&p)?;
        let hash = blob_hash(repo, &p)?;
        let a = parse_action(&p, &text, hash)?;
        actions.insert(a.name.clone(), a);
    }
    let mut workflows = BTreeMap::new();
    for p in toml_files(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let text = std::fs::read_to_string(&p)?;
        let hash = blob_hash(repo, &p)?;
        let w = parse_workflow(&p, &text, hash)?;
        workflows.insert(w.name.clone(), w);
    }
    let steps = job_steps(&wf, &workflows, &actions)?;
    Ok((wf, steps))
}

/// One blob at `rev` under `dir` in `repo`: its path relative to the
/// repository root and the git blob hash `git ls-tree` already carries —
/// the same content-addressed identity `blob_hash` derives from a working
/// copy (docs/WORKFLOWS.md, "Identity is the git blob hash of the file"),
/// here read straight out of the commit with no checkout.
fn ls_tree_dir(repo: &Path, rev: &str, dir: &str) -> Result<Vec<(PathBuf, String)>> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-tree", "-r", rev, "--", dir])
        .output()?;
    if !o.status.success() {
        bail!(
            "git ls-tree {rev} -- {dir} failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let hash = meta.split_whitespace().nth(2)?;
            Some((PathBuf::from(path), hash.to_string()))
        })
        .collect())
}

/// The bytes of one file at `rev` in `repo`, read with `git show`, no
/// checkout.
fn show_at(repo: &Path, rev: &str, path: &Path) -> Result<String> {
    let rel = path.to_str().context("path is not UTF-8")?;
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{rev}:{rel}")])
        .output()?;
    if !o.status.success() {
        bail!(
            "git show {rev}:{rel} failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Where a job's workflow was resolved from (docs/JOBS.md, "Where an
/// automation lives"): the project's own repository at its pinned commit,
/// tried first, or the operator's catalog when the repository holds no
/// workflow of that name there. Recorded on the job (`Job::workflow_source`)
/// so `forge job show` says which it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobSource {
    Repo,
    Catalog,
}

impl JobSource {
    pub fn as_str(self) -> &'static str {
        match self {
            JobSource::Repo => "repo",
            JobSource::Catalog => "catalog",
        }
    }
}

/// Every workflow under `.forge/workflows/*.toml` in a project's own
/// repository at `rev`, read with `git show` (no checkout, no working
/// tree) — for `forge workflows --project` (docs/JOBS.md, "Where an
/// automation lives"). Actions under `.forge/workflows/actions/` are not
/// themselves workflows and are skipped.
pub fn load_all_at(repo: &Path, rev: &str) -> Result<Vec<Workflow>> {
    let wf_dir = Path::new(".forge/workflows");
    let mut out = Vec::new();
    for (path, hash) in ls_tree_dir(repo, rev, wf_dir.to_str().unwrap())? {
        if path.parent() != Some(wf_dir) || path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        out.push(parse_workflow(&path, &text, hash)?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A run workflow resolved straight from a project's own repository at a
/// pinned commit — `.forge/workflows/<name>.toml`, read with `git show`,
/// no checkout — the same content-addressed pinning the operator's
/// catalog uses (docs/JOBS.md, "Where an automation lives"). Its steps
/// resolve against the operator's own action catalog (built-ins and
/// anything the operator has added) overlaid by the project's own
/// `.forge/workflows/actions/*.toml`: an automation names a built-in
/// effect operation directly, the way docs/JOBS.md's own example does, and
/// only needs a file of its own for an action the catalog does not have.
/// Every sibling `.forge/workflows/*.toml` at the same commit (`load_all_at`)
/// is parsed too, so a step that splices in another run workflow
/// (`job_steps`) resolves against the repository's own. `None` when the
/// repository has no workflow of this name at that commit, so
/// `resolve_job_for_project` falls back to the operator's catalog
/// entirely.
pub fn resolve_job_at(
    home: &Path,
    repo: &Path,
    rev: &str,
    name: &str,
) -> Result<Option<(Workflow, Vec<RunStep>)>> {
    let wf_path = Path::new(".forge/workflows").join(format!("{name}.toml"));
    let Some((path, hash)) = ls_tree_dir(repo, rev, wf_path.to_str().unwrap())?
        .into_iter()
        .find(|(p, _)| p == &wf_path)
    else {
        return Ok(None);
    };
    let text = show_at(repo, rev, &path)?;
    let wf = parse_workflow(&path, &text, hash)?;
    if wf.kind != WorkflowKind::Run {
        bail!(
            "{name:?} is kind = \"build\" in the project's repository; `forge job start` runs kind = \"run\" workflows only"
        );
    }
    let cat = load_catalog(home)?;
    ensure_sound(&cat)?;
    let mut actions = cat.actions;
    for (path, hash) in ls_tree_dir(repo, rev, ".forge/workflows/actions")? {
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        let a = parse_action(&path, &text, hash)?;
        actions.insert(a.name.clone(), a);
    }
    let workflows: BTreeMap<String, Workflow> = load_all_at(repo, rev)?
        .into_iter()
        .map(|w| (w.name.clone(), w))
        .collect();
    let steps = job_steps(&wf, &workflows, &actions)?;
    Ok(Some((wf, steps)))
}

/// A job's workflow, resolved the way `forge job start` does: first from
/// the project's own repository at `rev` (`resolve_job_at`), falling back
/// to the operator's catalog only when the repository has no workflow of
/// that name there (docs/JOBS.md, "Where an automation lives").
pub fn resolve_job_for_project(
    home: &Path,
    repo: &Path,
    rev: &str,
    name: &str,
) -> Result<(Workflow, Vec<RunStep>, JobSource)> {
    if let Some((wf, steps)) = resolve_job_at(home, repo, rev, name)? {
        return Ok((wf, steps, JobSource::Repo));
    }
    let (wf, steps) = resolve_job(home, name)?;
    Ok((wf, steps, JobSource::Catalog))
}

/// Every fixture under `.forge/fixtures/<workflow>/*.json` in a project's
/// own repository at `rev`, read the same way `resolve_job_at` reads the
/// workflow itself — `git show`, no checkout (docs/JOBS.md, "Verifying an
/// automation"). `(name, contents)`
/// pairs, sorted by name. No caller: `forge job test` reads the working
/// tree instead (`job::load_fixtures`), so a check sees work in progress;
/// this is the same discovery pinned to a commit.
#[allow(dead_code)]
pub fn fixtures_at(repo: &Path, rev: &str, workflow: &str) -> Result<Vec<(String, String)>> {
    let dir = Path::new(".forge/fixtures").join(workflow);
    let mut out = Vec::new();
    for (path, _hash) in ls_tree_dir(repo, rev, dir.to_str().context("path is not UTF-8")?)? {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = show_at(repo, rev, &path)?;
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        out.push((name, text));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// One problem `forge workflows validate` found in a repository's own
/// workflow or action file: the file, the line the parser could place it
/// at (a syntax or shape error has one; a semantic error found only after
/// a clean parse, like a step naming an unknown action, does not), and
/// the message.
#[derive(Debug, PartialEq, Eq)]
pub struct ValidateProblem {
    pub file: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

/// What `forge workflows validate` found under a path: how many workflow
/// and action files parsed and checked out clean, and every problem
/// otherwise.
pub struct ValidateReport {
    pub workflows: usize,
    pub actions: usize,
    pub problems: Vec<ValidateProblem>,
}

/// The 1-based line a TOML deserialize error points at, read off its span
/// (present for a syntax error or an unknown/mistyped field — exactly the
/// invented-format mistakes this command exists to catch); a `bail!` from
/// this module's own semantic checks, raised after a clean parse, carries
/// no span and gets no line.
fn error_line(err: &anyhow::Error, text: &str) -> Option<usize> {
    let span = err
        .chain()
        .find_map(|e| e.downcast_ref::<toml::de::Error>())
        .and_then(|e| e.span())?;
    Some(text[..span.start.min(text.len())].matches('\n').count() + 1)
}

/// `forge workflows validate`: load every `.forge/workflows/*.toml` and
/// `.forge/workflows/actions/*.toml` under `root` with the same parsers
/// the operator's catalog uses (`parse_action`, `parse_workflow`), so a
/// file the catalog cannot load fails the same way here as it would at
/// `forge job start` — but from a plain path, with no store and no
/// FORGE_HOME, so it runs as a repository check on any host that has the
/// `forge` binary (docs/WORKFLOWS.md, "Validating a repository's own
/// workflows"). A run workflow's steps must each resolve to a real action
/// (the repository's own or a built-in) and use `effect` on an operation
/// step only, the same rule `job_steps` enforces for `forge job start`; a
/// build workflow here only needs to parse, since a full catalog to
/// splice it against is not available offline.
pub fn validate_repo(root: &Path) -> Result<ValidateReport> {
    let wf_dir = root.join(".forge").join("workflows");
    let actions_dir = wf_dir.join("actions");

    let mut problems = Vec::new();
    let mut actions = builtin_actions_map()?;
    let mut n_actions = 0;
    for path in toml_files_if_present(&actions_dir)? {
        let text = std::fs::read_to_string(&path)?;
        let file = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        match parse_action(&path, &text, String::new()) {
            Ok(a) => {
                n_actions += 1;
                actions.insert(a.name.clone(), a);
            }
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, &text),
                file,
                message: format!("{e:#}"),
            }),
        }
    }

    // Parsed first, all of them, so a run workflow's step that splices in a
    // sibling run workflow (`job_steps`) resolves against every workflow
    // this repository declares, not just the one named on the command line.
    let mut n_workflows = 0;
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    let mut parsed: Vec<(PathBuf, String, Workflow)> = Vec::new();
    for path in toml_files_if_present(&wf_dir)? {
        let text = std::fs::read_to_string(&path)?;
        let file = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        match parse_workflow(&path, &text, String::new()) {
            Ok(wf) => {
                workflows.insert(wf.name.clone(), wf.clone());
                parsed.push((file, text, wf));
            }
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, &text),
                file,
                message: format!("{e:#}"),
            }),
        }
    }
    for (file, text, wf) in &parsed {
        let result = if wf.kind == WorkflowKind::Run {
            job_steps(wf, &workflows, &actions).map(|_| ())
        } else {
            Ok(())
        };
        match result {
            Ok(()) => n_workflows += 1,
            Err(e) => problems.push(ValidateProblem {
                line: error_line(&e, text),
                file: file.clone(),
                message: format!("{e:#}"),
            }),
        }
    }

    Ok(ValidateReport {
        workflows: n_workflows,
        actions: n_actions,
        problems,
    })
}

/// Every run workflow under `<root>/.forge/workflows`, each resolved the
/// way `validate_repo` resolves one — against the built-in actions and the
/// tree's own `.forge/workflows/actions/*.toml`, with no home directory —
/// for `forge job test` (docs/JOBS.md, "Verifying an automation"). `only`
/// narrows it to one workflow, refused when the tree has no run workflow
/// of that name. A workflow that does not resolve is returned as its
/// error rather than dropped, so a broken automation is a failure and not
/// a silent absence; a file that does not even parse is returned only
/// when `only` names it or fixtures exist for it, since without either it
/// cannot be told from a build workflow's typo. Sorted by name.
#[allow(clippy::type_complexity)]
pub fn resolve_jobs_in_tree(
    root: &Path,
    only: Option<&str>,
) -> Result<Vec<(String, Result<(Workflow, Vec<RunStep>)>)>> {
    let wf_dir = root.join(".forge").join("workflows");
    let mut actions = builtin_actions_map()?;
    for path in toml_files_if_present(&wf_dir.join("actions"))? {
        let text = std::fs::read_to_string(&path)?;
        let a = parse_action(&path, &text, String::new())?;
        actions.insert(a.name.clone(), a);
    }
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    let mut unparsed = Vec::new();
    for path in toml_files_if_present(&wf_dir)? {
        let text = std::fs::read_to_string(&path)?;
        match parse_workflow(&path, &text, String::new()) {
            Ok(wf) => {
                workflows.insert(wf.name.clone(), wf);
            }
            Err(e) => {
                let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                unparsed.push((stem, e));
            }
        }
    }
    let mut out = Vec::new();
    for (name, wf) in &workflows {
        if wf.kind != WorkflowKind::Run || only.is_some_and(|o| o != name) {
            continue;
        }
        let steps = job_steps(wf, &workflows, &actions).map(|steps| (wf.clone(), steps));
        out.push((name.clone(), steps));
    }
    for (name, e) in unparsed {
        let wanted = match only {
            Some(o) => o == name,
            None => root.join(".forge").join("fixtures").join(&name).is_dir(),
        };
        if wanted {
            out.push((name, Err(e)));
        }
    }
    if let Some(o) = only
        && out.is_empty()
    {
        if workflows.contains_key(o) {
            bail!(
                "{o:?} is kind = \"build\"; `forge job test` replays kind = \"run\" workflows only"
            );
        }
        bail!(
            "no run workflow {o:?} under {} (see docs/JOBS.md, \"Where an automation lives\")",
            wf_dir.display()
        );
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// One thing wrong with a file, and whether it blocks use.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub blocking: bool,
    pub what: String,
}

/// Every file, checked structurally; every workflow, resolved. Parse
/// errors are problems rather than errors, so one bad file does not hide
/// the rest. Deterministic: same files, same list.
pub fn check(home: &Path) -> Result<Vec<Problem>> {
    let Catalog {
        workflows,
        actions,
        mut problems,
    } = load_catalog(home)?;
    for (name, a) in &actions {
        if a.kind == Kind::Operation && KERNEL_OPS.contains(&name.as_str()) {
            problems.push(Problem {
                file: format!("actions/{name}.toml"),
                blocking: true,
                what: format!("operation {name:?} shadows a kernel operation"),
            });
        }
    }
    for (name, wf) in &workflows {
        // A run workflow does not follow the build data-flow rules
        // (`check_flow` requires a directive, which an operation-only job
        // never has); it only needs its steps' actions (and any spliced-in
        // sibling run workflow's) to exist (see `job_steps`).
        let r = if wf.kind == WorkflowKind::Run {
            job_steps(wf, &workflows, &actions).map(|_| ())
        } else {
            let mut out = Resolved::default();
            splice(wf, &workflows, &actions, &mut Vec::new(), &mut out)
                .and_then(|_| check_flow(&out.steps))
        };
        if let Err(e) = r {
            problems.push(Problem {
                file: format!("{name}.toml"),
                blocking: true,
                what: format!("{e:#}"),
            });
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    #[test]
    fn inline_composition_splices_and_rejects_cycles() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "outer.toml",
            "name = \"outer\"\ndescription = \"d\"\nsteps = [{ action = \"setup\" }, { workflow = \"tdd\" }]\n[meta]\nuse_when = \"u\"\navoid_when = \"a\"\n",
        );
        let r = resolve(dir.path(), "outer").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["setup", "tests", "setup", "repo-map", "code"]
        );
        assert_eq!(r.steps[1].via, vec!["outer", "tdd"]);
        assert!(
            r.pins
                .iter()
                .any(|p| p.kind == "workflow" && p.name == "tdd")
        );
        write(
            dir.path(),
            "a.toml",
            "name = \"a\"\nsteps = [{ workflow = \"b\" }]\n",
        );
        write(
            dir.path(),
            "b.toml",
            "name = \"b\"\nsteps = [{ workflow = \"a\" }]\n",
        );
        let err = resolve(dir.path(), "a").unwrap_err().to_string();
        assert!(err.contains("references itself"), "{err}");
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("references itself")),
            "{problems:?}"
        );
    }

    #[test]
    fn operations_produce_only_branch_or_interface_and_a_mutating_one_yields_a_verdict() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        for (name, body, want) in [
            (
                "verdicting",
                "produces = [\"verdict\"]\nrun = [\"true\"]\n",
                "cannot produce \"verdict\"",
            ),
            (
                "modelled",
                "model = \"haiku\"\nrun = [\"true\"]\n",
                "`model` applies to directives only",
            ),
        ] {
            write(
                dir.path(),
                &format!("actions/{name}.toml"),
                &format!(
                    "name = \"{name}\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"branch\"]\n{body}"
                ),
            );
            let err = load_actions(dir.path()).unwrap_err().to_string();
            assert!(err.contains(want), "{name}: {err}");
            std::fs::remove_file(
                dir.path()
                    .join("workflows/actions")
                    .join(format!("{name}.toml")),
            )
            .unwrap();
        }
        // The built-in fmt mutates, so polish (which consumes a verdict)
        // may follow it directly; the built-in interface reads the verify
        // ref and so needs the tests directive first.
        let fmt = load_actions(dir.path()).unwrap().remove("fmt").unwrap();
        assert!(fmt.mutates() && !fmt.yields_interface() && !fmt.reads_verify_ref());
        write(
            dir.path(),
            "fmt-polish.toml",
            "name = \"fmt-polish\"\nsteps = [{ action = \"fmt\" }, { action = \"polish\" }]\n",
        );
        assert!(resolve(dir.path(), "fmt-polish").is_ok());
        write(
            dir.path(),
            "iface-early.toml",
            "name = \"iface-early\"\nsteps = [{ action = \"interface\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "iface-early").unwrap_err().to_string();
        assert!(err.contains("consumes \"verify_ref\""), "{err}");
        write(
            dir.path(),
            "iface.toml",
            "name = \"iface\"\nsteps = [{ action = \"tests\" }, { action = \"interface\" }, { action = \"code\" }]\n",
        );
        let r = resolve(dir.path(), "iface").unwrap();
        assert!(r.steps[1].action.reads_verify_ref() && r.steps[1].action.yields_interface());
    }

    #[test]
    fn data_flow_and_kernel_shadowing_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/needs-ref.toml",
            "name = \"needs-ref\"\nkind = \"operation\"\ndescription = \"d\"\nconsumes = [\"verify_ref\"]\nrun = [\"true\"]\n",
        );
        write(
            dir.path(),
            "early.toml",
            "name = \"early\"\nsteps = [{ action = \"needs-ref\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "early").unwrap_err().to_string();
        assert!(err.contains("consumes \"verify_ref\""), "{err}");
        write(
            dir.path(),
            "late.toml",
            "name = \"late\"\nsteps = [{ action = \"tests\" }, { action = \"needs-ref\" }, { action = \"code\" }]\n",
        );
        assert!(resolve(dir.path(), "late").is_ok());
        write(
            dir.path(),
            "opsonly.toml",
            "name = \"opsonly\"\nsteps = [{ action = \"setup\" }]\n",
        );
        assert!(
            resolve(dir.path(), "opsonly")
                .unwrap_err()
                .to_string()
                .contains("at least one directive")
        );
        write(
            dir.path(),
            "actions/verify.toml",
            "name = \"verify\"\nkind = \"operation\"\ndescription = \"d\"\nrun = [\"true\"]\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("shadows a kernel operation")),
            "{problems:?}"
        );
        write(
            dir.path(),
            "actions/both.toml",
            "name = \"both\"\nkind = \"operation\"\nrun = [\"true\"]\ncheck = \"test\"\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems.iter().any(|p| p.what.contains("exactly one of")),
            "{problems:?}"
        );
    }

    #[test]
    fn typos_are_blocking_and_overrides_have_a_precedence() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/typo.toml",
            "name = \"typo\"\nkind = \"operation\"\ndescription = \"d\"\nrun = [\"true\"]\ntimeout_sec = 5\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems.iter().any(|p| p.blocking
                && p.file == "actions/typo.toml"
                && p.what.contains("unknown field")),
            "{problems:?}"
        );
        write(
            dir.path(),
            "wtypo.toml",
            "name = \"wtypo\"\nsteps = [{ action = \"code\", max_turn = 3 }]\n[meta]\nuse_wen = \"x\"\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.file == "wtypo.toml"),
            "{problems:?}"
        );
        std::fs::remove_file(dir.path().join("workflows/actions/typo.toml")).unwrap();
        std::fs::remove_file(dir.path().join("workflows/wtypo.toml")).unwrap();
        // step override > action default > task default (None here)
        write(
            dir.path(),
            "over.toml",
            "name = \"over\"\nsteps = [{ action = \"tests\", max_turns = 9, model = \"haiku\" }, { action = \"code\" }]\n",
        );
        let r = resolve(dir.path(), "over").unwrap();
        assert_eq!(r.steps[0].max_turns, Some(9));
        assert_eq!(r.steps[0].model.as_deref(), Some("haiku"));
        assert_eq!(r.steps[1].max_turns, None, "the task's own limit applies");
        // diamond: two paths to the same child splice twice, pin once
        write(
            dir.path(),
            "left.toml",
            "name = \"left\"\nsteps = [{ workflow = \"direct\" }]\n",
        );
        write(
            dir.path(),
            "right.toml",
            "name = \"right\"\nsteps = [{ workflow = \"direct\" }]\n",
        );
        write(
            dir.path(),
            "diamond.toml",
            "name = \"diamond\"\nsteps = [{ workflow = \"left\" }, { workflow = \"right\" }]\n",
        );
        let r = resolve(dir.path(), "diamond").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["setup", "repo-map", "code", "setup", "repo-map", "code"]
        );
        assert_eq!(r.pins.iter().filter(|p| p.name == "direct").count(), 1);
        assert_eq!(r.pins.iter().filter(|p| p.name == "code").count(), 1);
    }

    #[test]
    fn verifying_operations_need_a_preceding_directive() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        let a = load_actions(dir.path()).unwrap();
        assert!(a["playwright"].overlay && a["playwright"].verifies);
        write(
            dir.path(),
            "early.toml",
            "name = \"early\"\nsteps = [{ action = \"playwright\" }, { action = \"code\" }]\n",
        );
        let err = resolve(dir.path(), "early").unwrap_err().to_string();
        assert!(err.contains("no directive precedes it"), "{err}");
        write(
            dir.path(),
            "actions/badd.toml",
            "name = \"badd\"\nkind = \"directive\"\ncontract = \"code\"\noverlay = true\n",
        );
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|p| p.what.contains("operations only"))
        );
    }

    /// A run workflow's step may name a sibling run workflow instead of an
    /// action, spliced inline recursively (`job_steps`) — the composition
    /// `doctor-daily.toml` uses to pull in `disk-and-logs.toml`. Splicing a
    /// `kind = "build"` workflow, or a cycle of run workflows, is refused.
    #[test]
    fn resolve_job_splices_a_sibling_run_workflow_and_rejects_a_build_kind_child_or_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "inner-run.toml",
            "name = \"inner-run\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ action = \"fmt\" }]\n[trigger]\non = \"manual\"\n",
        );
        write(
            dir.path(),
            "outer-run.toml",
            "name = \"outer-run\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ action = \"fmt\" }, { workflow = \"inner-run\" }]\n[trigger]\non = \"manual\"\n",
        );
        let (wf, steps) = resolve_job(dir.path(), "outer-run").unwrap();
        assert_eq!(wf.kind, WorkflowKind::Run);
        assert_eq!(
            steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["fmt", "fmt"]
        );

        write(
            dir.path(),
            "build-child.toml",
            "name = \"build-child\"\ndescription = \"d\"\nsteps = [{ action = \"code\" }]\n",
        );
        write(
            dir.path(),
            "bad-splice.toml",
            "name = \"bad-splice\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"build-child\" }]\n[trigger]\non = \"manual\"\n",
        );
        let err = resolve_job(dir.path(), "bad-splice")
            .unwrap_err()
            .to_string();
        assert!(err.contains("kind = \"build\""), "{err}");

        write(
            dir.path(),
            "cycle-a.toml",
            "name = \"cycle-a\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"cycle-b\" }]\n[trigger]\non = \"manual\"\n",
        );
        write(
            dir.path(),
            "cycle-b.toml",
            "name = \"cycle-b\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{ workflow = \"cycle-a\" }]\n[trigger]\non = \"manual\"\n",
        );
        let err = resolve_job(dir.path(), "cycle-a").unwrap_err().to_string();
        assert!(err.contains("references itself"), "{err}");
    }

    fn git(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    #[test]
    fn resolve_job_at_reads_a_run_workflow_from_the_repository_at_a_pinned_commit_merging_operator_and_project_actions()
     {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        git(r, &["init", "-q", "-b", "main"]);
        git(r, &["config", "user.email", "t@example.com"]);
        git(r, &["config", "user.name", "t"]);
        std::fs::create_dir_all(r.join(".forge/workflows/actions")).unwrap();
        std::fs::create_dir_all(r.join(".forge/fixtures/publish-snapshot")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            r#"name = "publish-snapshot"
kind = "run"
description = "writes a file with a built-in operation and logs with the project's own"

steps = [
  { action = "write-file", effect = "file" },
  { action = "custom-op",  effect = "row" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/actions/custom-op.toml"),
            "name = \"custom-op\"\nkind = \"operation\"\ndescription = \"the project's own operation\"\nrun = [\"true\"]\n",
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/fixtures/publish-snapshot/01-example.json"),
            "{\"a\":1}\n",
        )
        .unwrap();
        git(r, &["add", "-A"]);
        git(r, &["commit", "-q", "-m", "automation"]);
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let sha = String::from_utf8_lossy(&o.stdout).trim().to_string();

        let home = tempfile::tempdir().unwrap();
        load_all(home.path()).unwrap();

        let (wf, steps) = resolve_job_at(home.path(), r, &sha, "publish-snapshot")
            .unwrap()
            .unwrap();
        assert_eq!(wf.kind, WorkflowKind::Run);
        let names: Vec<&str> = steps.iter().map(|s| s.action.name.as_str()).collect();
        assert_eq!(names, vec!["write-file", "custom-op"]);

        // A name the repository does not have at that commit: no fallback
        // to the catalog inside `resolve_job_at` itself.
        assert!(
            resolve_job_at(home.path(), r, &sha, "no-such-workflow")
                .unwrap()
                .is_none()
        );

        let (_, _, source) =
            resolve_job_for_project(home.path(), r, &sha, "publish-snapshot").unwrap();
        assert_eq!(source, JobSource::Repo);
        // Falls to the operator's catalog when the repository has none by
        // that name.
        write(
            home.path(),
            "catalog-only.toml",
            r#"name = "catalog-only"
kind = "run"
description = "a run workflow the operator's catalog carries but the repository does not"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]
"#,
        );
        let (_, _, source) = resolve_job_for_project(home.path(), r, &sha, "catalog-only").unwrap();
        assert_eq!(source, JobSource::Catalog);

        let all = load_all_at(r, &sha).unwrap();
        assert_eq!(
            all.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["publish-snapshot"]
        );

        let fixtures = fixtures_at(r, &sha, "publish-snapshot").unwrap();
        assert_eq!(fixtures.len(), 1);
        assert_eq!(fixtures[0].0, "01-example");
        assert!(fixtures[0].1.contains("\"a\""));
    }

    #[test]
    fn validate_repo_accepts_a_clean_tree_with_no_store_and_no_forge_home() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows/actions")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            r#"name = "publish-snapshot"
kind = "run"
description = "writes a file with a built-in operation and logs with the project's own"

steps = [
  { action = "write-file", effect = "file" },
  { action = "custom-op",  effect = "row" },
]

[trigger]
on = "manual"

[assert]
noop = ["true"]

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/actions/custom-op.toml"),
            "name = \"custom-op\"\nkind = \"operation\"\ndescription = \"the project's own operation\"\nrun = [\"true\"]\n",
        )
        .unwrap();

        // Reading FORGE_HOME here would panic the test process (it does not
        // exist); `validate_repo` takes only `r`, never a home.
        let report = validate_repo(r).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.workflows, 1);
        assert_eq!(report.actions, 1);
    }

    /// `doctor-daily.toml` splicing in `disk-and-logs.toml` (docs/JOBS.md,
    /// "Steps"): a run workflow's step may name a sibling run workflow
    /// instead of an action, and `forge workflows validate` resolves it
    /// against every workflow the repository declares, not just the one
    /// named.
    #[test]
    fn validate_repo_accepts_a_run_workflow_splicing_a_sibling_run_workflow() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/outer.toml"),
            "name = \"outer\"\nkind = \"run\"\ndescription = \"d\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n  { workflow = \"inner\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();
        std::fs::write(
            r.join(".forge/workflows/inner.toml"),
            "name = \"inner\"\nkind = \"run\"\ndescription = \"d\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.workflows, 2);
    }

    #[test]
    fn validate_repo_accepts_an_empty_or_absent_forge_directory() {
        let repo = tempfile::tempdir().unwrap();
        let report = validate_repo(repo.path()).unwrap();
        assert!(report.problems.is_empty());
        assert_eq!(report.workflows, 0);
        assert_eq!(report.actions, 0);
    }

    /// The three invented shapes equitizr's `.forge/workflows/publish-snapshot.toml`
    /// landed twice, that a repository's own checks never validated
    /// because nothing ran the catalog's loader against it.
    #[test]
    fn validate_repo_catches_a_string_trigger() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented string trigger\"\ntrigger = \"manual\"\n\nsteps = [\n  { action = \"write-file\", effect = \"file\" },\n]\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(
            p.file,
            PathBuf::from(".forge/workflows/publish-snapshot.toml")
        );
        assert_eq!(p.line, Some(4), "{}", p.message);
        assert!(p.message.contains("invalid type"), "{}", p.message);
    }

    #[test]
    fn validate_repo_catches_a_steps_table_with_an_inline_run_command() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented [[steps]] table with an inline run command\"\n\n[[steps]]\nrun = \"echo hi\"\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(p.line, Some(6), "{}", p.message);
        assert!(p.message.contains("unknown field `run`"), "{}", p.message);
    }

    #[test]
    fn validate_repo_catches_an_http_get_step_with_url_and_field() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"invented http_get step with url and field\"\n\nsteps = [\n  { action = \"http_get\", url = \"https://example.com\", field = \"x\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(p.line, Some(6), "{}", p.message);
        assert!(
            p.message.contains("unknown field `url`")
                || p.message.contains("unknown field `field`"),
            "{}",
            p.message
        );
    }

    #[test]
    fn validate_repo_catches_a_step_referencing_an_unknown_action() {
        let repo = tempfile::tempdir().unwrap();
        let r = repo.path();
        std::fs::create_dir_all(r.join(".forge/workflows")).unwrap();
        std::fs::write(
            r.join(".forge/workflows/publish-snapshot.toml"),
            "name = \"publish-snapshot\"\nkind = \"run\"\ndescription = \"references an action nothing declares\"\n\nsteps = [\n  { action = \"does-not-exist\", effect = \"file\" },\n]\n\n[trigger]\non = \"manual\"\n",
        )
        .unwrap();

        let report = validate_repo(r).unwrap();
        assert_eq!(report.workflows, 0);
        assert_eq!(report.problems.len(), 1);
        let p = &report.problems[0];
        assert_eq!(
            p.line, None,
            "a semantic check after a clean parse has no span"
        );
        assert!(p.message.contains("does-not-exist"), "{}", p.message);
    }
}
