//! Workflows and actions are data: one TOML file each under
//! `<FORGE2_HOME>/workflows/` (workflows) and `.../workflows/actions/`
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
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Names the engine inserts itself; a user operation may not shadow them.
pub const KERNEL_OPS: &[&str] = &["verify", "push", "integrate", "clone"];

/// Directives the kernel has a contract for. A directive file with another
/// name is a parameter sheet over nothing and is rejected until custom
/// directives exist (docs/ACTIONS.md).
pub const KNOWN_DIRECTIVES: &[&str] = &["code", "tests"];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Directive,
    Operation,
}

#[derive(Deserialize)]
struct ActionRaw {
    name: String,
    kind: Kind,
    #[serde(default)]
    description: String,
    #[serde(default)]
    consumes: Vec<String>,
    #[serde(default)]
    produces: Vec<String>,
    model: Option<String>,
    max_turns: Option<u32>,
    timeout_secs: Option<u32>,
    /// Operation: a command to run in the sandbox against the tree.
    run: Option<Vec<String>>,
    /// Operation: run the repository's declared check of this name instead.
    check: Option<String>,
}

/// One action file, one version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionDef {
    pub name: String,
    pub kind: Kind,
    pub description: String,
    pub consumes: Vec<String>,
    pub produces: Vec<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub run: Option<Vec<String>>,
    pub check: Option<String>,
    pub hash: String,
    pub text: String,
}

#[derive(Deserialize, Clone, Debug)]
struct StepRaw {
    action: Option<String>,
    workflow: Option<String>,
    /// Old spelling of `action`, accepted for one release.
    kind: Option<String>,
    model: Option<String>,
    max_turns: Option<u32>,
    timeout_secs: Option<u32>,
}

#[derive(Deserialize)]
struct WorkflowRaw {
    name: String,
    #[serde(default)]
    description: String,
    steps: Vec<StepRaw>,
    #[serde(default)]
    meta: Meta,
}

/// What the author declares about a workflow, for a human or an agent
/// choosing one. Declared, never measured: measured numbers live in the
/// stats table and are merged in at read time.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Meta {
    #[serde(default)]
    pub use_when: String,
    #[serde(default)]
    pub avoid_when: String,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Expected cost relative to `direct` (1.0). A rough prior, not a measurement.
    #[serde(default = "one")]
    pub cost_factor: f64,
}

fn one() -> f64 {
    1.0
}

impl Default for Meta {
    fn default() -> Meta {
        Meta {
            use_when: String::new(),
            avoid_when: String::new(),
            requires: Vec::new(),
            cost_factor: 1.0,
        }
    }
}

/// A step as written: a reference plus per-step overrides.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepRef {
    pub action: Option<String>,
    pub workflow: Option<String>,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub steps: Vec<StepRef>,
    pub meta: Meta,
    pub hash: String,
    pub path: PathBuf,
    pub text: String,
}

/// A step after resolution: the exact action version it will run, with
/// the overrides that apply, and the workflow path it came through.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedStep {
    pub action: ActionDef,
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub via: Vec<String>,
}

/// One version that a task ran under.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pin {
    pub kind: String,
    pub name: String,
    pub hash: String,
}

/// Everything a task needs to run, recorded at creation.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Resolved {
    pub steps: Vec<ResolvedStep>,
    pub pins: Vec<Pin>,
}

impl Workflow {
    pub fn steps_text(&self) -> String {
        self.steps
            .iter()
            .map(|s| {
                s.action
                    .clone()
                    .or_else(|| s.workflow.as_ref().map(|w| format!("({w})")))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

const BUILTIN_ACTIONS: &[(&str, &str)] = &[
    (
        "code.toml",
        "name = \"code\"\n\
kind = \"directive\"\n\
description = \"an agent writes the change in the task's clone; may not touch protected paths or the verification namespace\"\n\
consumes = [\"branch\"]\n\
produces = [\"branch\"]\n",
    ),
    (
        "tests.toml",
        "name = \"tests\"\n\
kind = \"directive\"\n\
description = \"an agent writes tests only inside the verification namespace that fail on base; its summary is the interface the coder sees\"\n\
consumes = [\"branch\"]\n\
produces = [\"verify_ref\", \"interface\"]\n\
max_turns = 40\n",
    ),
    (
        "setup.toml",
        "name = \"setup\"\n\
kind = \"operation\"\n\
description = \"run the repository's setup check in the clone so the next directive starts check-ready\"\n\
consumes = [\"branch\"]\n\
produces = []\n\
check = \"setup\"\n\
timeout_secs = 600\n",
    ),
];

const BUILTIN_WORKFLOWS: &[(&str, &str)] = &[
    (
        "direct.toml",
        "name = \"direct\"\n\
description = \"one agent writes the change; the kernel verifies\"\n\
steps = [\n\
  { action = \"setup\" },\n\
  { action = \"code\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task is small and precisely described, and the repo's own checks cover it\"\n\
avoid_when = \"the task's correctness is not captured by existing tests and no --check can express it\"\n\
requires = []\n\
cost_factor = 1.0\n",
    ),
    (
        "tdd.toml",
        "name = \"tdd\"\n\
description = \"one agent writes hidden tests that fail on base; another makes them pass seeing only the interface\"\n\
steps = [\n\
  { action = \"tests\" },\n\
  { action = \"setup\" },\n\
  { action = \"code\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task adds behavior that a test can pin down and the repo's checks would not otherwise catch a wrong implementation\"\n\
avoid_when = \"the task is a refactor, a rename, docs, or config; or the repo has no test check\"\n\
requires = [\"[verify] namespace in forge.toml\", \"a check named test\"]\n\
cost_factor = 2.5\n",
    ),
];

fn dir_of(home: &Path) -> PathBuf {
    home.join("workflows")
}

/// The directory exists, is a git repository, and holds the built-ins if
/// it holds nothing at all.
fn ensure(home: &Path) -> Result<PathBuf> {
    let dir = dir_of(home);
    let actions = dir.join("actions");
    std::fs::create_dir_all(&actions)?;
    if !dir.join(".git").exists() {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["init", "-q"])
            .output()?;
        if !o.status.success() {
            bail!(
                "git init in {} failed: {}",
                dir.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
    let has = |d: &Path| {
        std::fs::read_dir(d)
            .map(|r| {
                r.filter_map(|e| e.ok())
                    .any(|e| e.path().extension().is_some_and(|x| x == "toml"))
            })
            .unwrap_or(false)
    };
    if !has(&dir) {
        for (file, text) in BUILTIN_WORKFLOWS {
            std::fs::write(dir.join(file), text)?;
        }
    }
    if !has(&actions) {
        for (file, text) in BUILTIN_ACTIONS {
            std::fs::write(actions.join(file), text)?;
        }
    }
    Ok(dir)
}

/// The git blob hash of a file: the identity git gives this version.
fn blob_hash(dir: &Path, path: &Path) -> Result<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("hash-object")
        .arg(path)
        .output()?;
    if !o.status.success() {
        bail!(
            "git hash-object {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn parse_action(dir: &Path, path: &Path, text: &str) -> Result<ActionDef> {
    let raw: ActionRaw =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    match raw.kind {
        Kind::Operation => {
            if raw.run.is_none() == raw.check.is_none() {
                bail!(
                    "{}: an operation needs exactly one of `run` or `check`",
                    path.display()
                );
            }
            if raw.run.as_ref().is_some_and(|r| r.is_empty()) {
                bail!("{}: `run` is empty", path.display());
            }
        }
        Kind::Directive => {
            if raw.run.is_some() || raw.check.is_some() {
                bail!(
                    "{}: a directive does not have `run` or `check`",
                    path.display()
                );
            }
        }
    }
    Ok(ActionDef {
        name: raw.name,
        kind: raw.kind,
        description: raw.description,
        consumes: raw.consumes,
        produces: raw.produces,
        model: raw.model,
        max_turns: raw.max_turns,
        timeout_secs: raw.timeout_secs,
        run: raw.run,
        check: raw.check,
        hash: blob_hash(dir, path)?,
        text: text.to_string(),
    })
}

fn parse_workflow(dir: &Path, path: &Path, text: &str) -> Result<Workflow> {
    let raw: WorkflowRaw =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    if raw.steps.is_empty() {
        bail!("{}: a workflow needs at least one step", path.display());
    }
    let mut steps = Vec::new();
    for s in raw.steps {
        let action = s.action.or(s.kind);
        if action.is_some() == s.workflow.is_some() {
            bail!(
                "{}: a step names exactly one of `action` or `workflow`",
                path.display()
            );
        }
        steps.push(StepRef {
            action,
            workflow: s.workflow,
            model: s.model,
            max_turns: s.max_turns,
            timeout_secs: s.timeout_secs,
        });
    }
    Ok(Workflow {
        name: raw.name,
        description: raw.description,
        steps,
        meta: raw.meta,
        hash: blob_hash(dir, path)?,
        path: path.to_path_buf(),
        text: text.to_string(),
    })
}

fn toml_files(d: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(d)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    v.sort();
    Ok(v)
}

/// Every action file, by name.
pub fn load_actions(home: &Path) -> Result<BTreeMap<String, ActionDef>> {
    let dir = ensure(home)?;
    let mut out = BTreeMap::new();
    for path in toml_files(&dir.join("actions"))? {
        let text = std::fs::read_to_string(&path)?;
        let a = parse_action(&dir, &path, &text)?;
        out.insert(a.name.clone(), a);
    }
    Ok(out)
}

/// Every workflow, sorted by name.
pub fn load_all(home: &Path) -> Result<Vec<Workflow>> {
    let dir = ensure(home)?;
    let mut out = Vec::new();
    for path in toml_files(&dir)? {
        let text = std::fs::read_to_string(&path)?;
        out.push(parse_workflow(&dir, &path, &text)?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn get(home: &Path, name: &str) -> Result<Option<Workflow>> {
    Ok(load_all(home)?.into_iter().find(|w| w.name == name))
}

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
            if a.kind == Kind::Directive && !KNOWN_DIRECTIVES.contains(&a.name.as_str()) {
                bail!(
                    "directive {:?} has no kernel contract (known: {})",
                    a.name,
                    KNOWN_DIRECTIVES.join(", ")
                );
            }
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
    let mut have: BTreeSet<&str> = ["branch"].into_iter().collect();
    for (i, s) in steps.iter().enumerate() {
        for c in &s.action.consumes {
            if !have.contains(c.as_str()) {
                bail!(
                    "step {} ({}) consumes {:?}, which nothing before it produces (have: {})",
                    i + 1,
                    s.action.name,
                    c,
                    have.iter().copied().collect::<Vec<_>>().join(", ")
                );
            }
        }
        for p in &s.action.produces {
            have.insert(p.as_str());
        }
        if s.action.kind == Kind::Directive {
            have.insert("verdict");
        }
    }
    if !steps.iter().any(|s| s.action.kind == Kind::Directive) {
        bail!("a workflow needs at least one directive; operations alone produce nothing to push");
    }
    Ok(())
}

/// Resolve a workflow by name into the exact versions a task will run,
/// splicing referenced workflows inline. Fails on unknown references,
/// cycles, and data-flow violations.
pub fn resolve(home: &Path, name: &str) -> Result<Resolved> {
    let workflows: BTreeMap<String, Workflow> = load_all(home)?
        .into_iter()
        .map(|w| (w.name.clone(), w))
        .collect();
    let actions = load_actions(home)?;
    let wf = workflows
        .get(name)
        .with_context(|| format!("unknown workflow {name:?}; see `forge workflows`"))?;
    let mut out = Resolved::default();
    splice(wf, &workflows, &actions, &mut Vec::new(), &mut out)?;
    check_flow(&out.steps)?;
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
    let dir = ensure(home)?;
    let mut problems = Vec::new();
    let mut actions: BTreeMap<String, ActionDef> = BTreeMap::new();
    for path in toml_files(&dir.join("actions"))? {
        let file = format!("actions/{}", path.file_name().unwrap().to_string_lossy());
        let text = std::fs::read_to_string(&path)?;
        let a = match parse_action(&dir, &path, &text) {
            Ok(a) => a,
            Err(e) => {
                problems.push(Problem {
                    file,
                    blocking: true,
                    what: format!("{e:#}"),
                });
                continue;
            }
        };
        let stem = path.file_stem().unwrap().to_string_lossy();
        if a.name != stem {
            problems.push(Problem {
                file: file.clone(),
                blocking: true,
                what: format!("name {:?} does not match the file name {:?}", a.name, stem),
            });
        }
        if a.kind == Kind::Operation && KERNEL_OPS.contains(&a.name.as_str()) {
            problems.push(Problem {
                file: file.clone(),
                blocking: true,
                what: format!("operation {:?} shadows a kernel operation", a.name),
            });
        }
        if a.kind == Kind::Directive && !KNOWN_DIRECTIVES.contains(&a.name.as_str()) {
            problems.push(Problem {
                file: file.clone(),
                blocking: true,
                what: format!("directive {:?} has no kernel contract (known: {}); custom directives are not supported yet", a.name, KNOWN_DIRECTIVES.join(", ")),
            });
        }
        if a.max_turns == Some(0) || a.timeout_secs == Some(0) {
            problems.push(Problem {
                file: file.clone(),
                blocking: true,
                what: "max_turns and timeout_secs must be positive".into(),
            });
        }
        if a.description.trim().is_empty() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "no description".into(),
            });
        }
        actions.insert(a.name.clone(), a);
    }
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    for path in toml_files(&dir)? {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path)?;
        let w = match parse_workflow(&dir, &path, &text) {
            Ok(w) => w,
            Err(e) => {
                problems.push(Problem {
                    file,
                    blocking: true,
                    what: format!("{e:#}"),
                });
                continue;
            }
        };
        let stem = path.file_stem().unwrap().to_string_lossy();
        if w.name != stem {
            problems.push(Problem {
                file: file.clone(),
                blocking: true,
                what: format!("name {:?} does not match the file name {:?}", w.name, stem),
            });
        }
        if w.meta.cost_factor <= 0.0 || w.meta.cost_factor.is_nan() {
            problems.push(Problem {
                file: file.clone(),
                blocking: true,
                what: format!("cost_factor must be positive, got {}", w.meta.cost_factor),
            });
        }
        if w.description.trim().is_empty() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "no description".into(),
            });
        }
        if w.meta.use_when.trim().is_empty() || w.meta.avoid_when.trim().is_empty() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "[meta] use_when and avoid_when are empty; a chooser has nothing to read"
                    .into(),
            });
        }
        for s in &w.steps {
            if s.max_turns == Some(0) || s.timeout_secs == Some(0) {
                problems.push(Problem {
                    file: file.clone(),
                    blocking: true,
                    what: "max_turns and timeout_secs must be positive".into(),
                });
            }
        }
        workflows.insert(w.name.clone(), w);
    }
    for (name, wf) in &workflows {
        let mut out = Resolved::default();
        let r = splice(wf, &workflows, &actions, &mut Vec::new(), &mut out)
            .and_then(|_| check_flow(&out.steps));
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

/// Files changed since the last commit of the directory, or all files if
/// it has never been committed.
pub fn uncommitted(home: &Path) -> Result<Vec<String>> {
    let dir = ensure(home)?;
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()?;
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| l[3..].to_string())
        .collect())
}

/// The commit that introduced a blob into the directory's history, if it
/// has been committed. For "which commit do I check out to revert".
pub fn commit_for(home: &Path, hash: &str) -> Option<String> {
    let dir = dir_of(home);
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["log", "--format=%h %cs", "--find-object", hash, "--reverse"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .next()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    #[test]
    fn builtins_resolve_and_carry_blob_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let all = load_all(dir.path()).unwrap();
        assert_eq!(
            all.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["direct", "tdd"]
        );
        let r = resolve(dir.path(), "tdd").unwrap();
        assert_eq!(
            r.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tests", "setup", "code"]
        );
        assert_eq!(
            r.steps[0].max_turns,
            Some(40),
            "the action's own default applies"
        );
        assert_eq!(r.steps[1].action.kind, Kind::Operation);
        assert_eq!(r.pins.len(), 4, "the workflow and three actions");
        assert!(r.pins.iter().all(|p| p.hash.len() == 40), "git blob hashes");
        assert!(check(dir.path()).unwrap().iter().all(|p| !p.blocking));
        // Editing a file changes only its own version.
        let before = resolve(dir.path(), "direct").unwrap();
        write(
            dir.path(),
            "actions/code.toml",
            "name = \"code\"\nkind = \"directive\"\ndescription = \"x\"\nconsumes = [\"branch\"]\nproduces = [\"branch\"]\nmax_turns = 50\n",
        );
        let after = resolve(dir.path(), "direct").unwrap();
        assert_ne!(before.pins, after.pins);
        assert_eq!(after.steps[1].max_turns, Some(50));
        assert_eq!(
            before.pins.iter().find(|p| p.name == "setup"),
            after.pins.iter().find(|p| p.name == "setup"),
            "setup is unchanged"
        );
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
            vec!["setup", "tests", "setup", "code"]
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
    fn unknown_directives_are_rejected_until_contracts_exist() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/review.toml",
            "name = \"review\"\nkind = \"directive\"\ndescription = \"d\"\nconsumes = [\"branch\"]\n",
        );
        write(
            dir.path(),
            "rev.toml",
            "name = \"rev\"\nsteps = [{ action = \"code\" }, { action = \"review\" }]\n",
        );
        let err = resolve(dir.path(), "rev").unwrap_err().to_string();
        assert!(err.contains("no kernel contract"), "{err}");
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|p| p.blocking && p.what.contains("no kernel contract"))
        );
    }

    #[test]
    fn old_kind_spelling_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "old.toml",
            "name = \"old\"\nsteps = [{ kind = \"code\" }]\n",
        );
        assert_eq!(
            resolve(dir.path(), "old").unwrap().steps[0].action.name,
            "code"
        );
    }
}
