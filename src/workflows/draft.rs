//! The draft editor's model (docs/WORKFLOWS.md, "The draft editor"): a
//! workflow as data — a step list with failure edges — that renders to the
//! same TOML the catalog loads and is linted with the catalog's own linter
//! on every change, so a draft is always clean or annotated.
//!
//! A step may name an action that does not exist yet. The draft then
//! carries a [`Placeholder`], a one-line contract the operator writes
//! (inputs, outputs, kind), the linter is given a stand-in action of that
//! kind so the rest of the flow is still checked, and the draft is saved
//! `incomplete` until every placeholder's action has landed in the catalog
//! and the lint passes ([`transition`]), when it becomes `enabled` on its
//! own ([`reconcile`]). A draft lives beside the catalog as
//! `<home>/drafts/<name>.json`: an incomplete draft cannot be a catalog
//! file, since a workflow naming an unknown action fails the catalog load.

use super::lint::{known_actions, lint_with};
use super::{ActionDef, Kind, LintProblem, Workflow, WorkflowKind, parse_action};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where saved drafts live, under the home directory.
pub const DIR: &str = "drafts";

/// Where a draft stands. `Draft` has no unbuilt action; `Incomplete` was
/// put with placeholders whose actions have not all landed; `Enabled` is
/// in the catalog.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Draft,
    Incomplete,
    Enabled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::Incomplete => "incomplete",
            Status::Enabled => "enabled",
        }
    }
}

/// The one-line contract of an action that does not exist yet: what it
/// takes, what it gives, and whether it is an operation (a script) or a
/// directive (a model step).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Placeholder {
    pub kind: Kind,
    pub inputs: String,
    pub outputs: String,
}

impl Placeholder {
    /// `inputs: ...; outputs: ...; kind: operation`, the text the build
    /// task and the linter's stand-in both carry.
    pub fn line(&self) -> String {
        format!(
            "inputs: {}; outputs: {}; kind: {}",
            self.inputs.trim(),
            self.outputs.trim(),
            kind_word(self.kind)
        )
    }
}

fn kind_word(k: Kind) -> &'static str {
    match k {
        Kind::Operation => "operation",
        Kind::Directive => "directive",
    }
}

/// One step: an action (or another workflow, in `extra`), how it is run,
/// and where a failure or outcome goes. Fields the editor does not model
/// (`model`, `max_turns`, `workflow`, ...) ride along in `extra` untouched.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct DraftStep {
    #[serde(default)]
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// Edges: `failure` or an outcome, to a step name, node id or `end`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub on: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<Placeholder>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A build task filed for one placeholder's action.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Filed {
    pub action: String,
    pub task_id: i64,
}

/// The whole draft as data. `settings` is every top-level key the step
/// editor does not model (`trigger`, `limits`, `assert`, `meta`, ...),
/// kept as JSON and written back as TOML.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub name: String,
    #[serde(default)]
    pub kind: WorkflowKind,
    #[serde(default)]
    pub description: String,
    /// The project the build tasks of a placeholder are filed on.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub steps: Vec<DraftStep>,
    #[serde(default)]
    pub settings: Map<String, Value>,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub tasks: Vec<Filed>,
    /// A one-shot suggestion's proposed steps, shown for the operator to
    /// edit and accept; never part of the rendered workflow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposal: Vec<DraftStep>,
}

/// A problem in a draft: a lint problem placed on its step when its line is
/// a step's line, or a placeholder rule violation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Problem {
    pub line: Option<usize>,
    pub step: Option<usize>,
    pub message: String,
}

/// What the editor shows beside the step list.
#[derive(Clone, Debug, Serialize)]
pub struct StepInfo {
    pub kind: Option<Kind>,
    pub contract: Option<String>,
    pub description: String,
    pub placeholder: bool,
    /// The placeholder's action exists now.
    pub landed: bool,
}

/// A draft with the result of linting it: the file it renders to, every
/// problem, per-step contract info, and the status it would have.
#[derive(Clone, Debug, Serialize)]
pub struct Annotated {
    #[serde(flatten)]
    pub draft: Draft,
    pub toml: String,
    pub problems: Vec<Problem>,
    pub info: Vec<StepInfo>,
    pub clean: bool,
    /// The placeholders whose action is still missing, one per action.
    pub pending: Vec<String>,
}

/// A name a draft may be saved under: a file stem in the catalog.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The status a draft moves to: an `Incomplete` draft becomes `Enabled` on
/// its own once no placeholder's action is missing and the lint passes;
/// every other status stays where it is.
pub fn transition(status: Status, pending: usize, lint_clean: bool) -> Status {
    match status {
        Status::Incomplete if pending == 0 && lint_clean => Status::Enabled,
        s => s,
    }
}

/// The status a draft is saved with: `Incomplete` while any placeholder's
/// action is missing (even a re-edited, already enabled draft, so `reconcile`
/// re-enables it once the action lands), else what it already was.
pub fn saved_status(status: Status, pending: usize) -> Status {
    if pending > 0 {
        Status::Incomplete
    } else {
        status
    }
}

fn json_to_toml(v: &Value) -> String {
    match v {
        Value::String(s) => toml::Value::String(s.clone()).to_string(),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(json_to_toml).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => inline(o.iter().map(|(k, v)| (k.as_str(), json_to_toml(v)))),
        other => other.to_string(),
    }
}

fn key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_string()
    } else {
        toml::Value::String(k.to_string()).to_string()
    }
}

fn inline<'a>(pairs: impl Iterator<Item = (&'a str, String)>) -> String {
    let body: Vec<String> = pairs.map(|(k, v)| format!("{} = {v}", key(k))).collect();
    format!("{{ {} }}", body.join(", "))
}

impl DraftStep {
    pub fn named(action: &str) -> DraftStep {
        DraftStep {
            action: action.to_string(),
            ..Default::default()
        }
    }

    /// One inline table, the shape the catalog's own files use.
    fn render(&self) -> String {
        let mut pairs: Vec<(&str, String)> = Vec::new();
        let s = |v: &str| toml::Value::String(v.to_string()).to_string();
        if !self.action.is_empty() {
            pairs.push(("action", s(&self.action)));
        }
        for (k, v) in [
            ("role", &self.role),
            ("judgment", &self.judgment),
            ("effect", &self.effect),
        ] {
            if let Some(v) = v.as_deref().filter(|v| !v.trim().is_empty()) {
                pairs.push((k, s(v)));
            }
        }
        for (k, v) in &self.extra {
            pairs.push((k, json_to_toml(v)));
        }
        if !self.on.is_empty() {
            let edges = inline(self.on.iter().map(|(k, v)| (k.as_str(), s(v))));
            pairs.push(("on", edges));
        }
        inline(pairs.into_iter())
    }
}

impl Draft {
    pub fn new(name: &str, kind: WorkflowKind) -> Draft {
        let mut settings = Map::new();
        if kind == WorkflowKind::Run {
            settings.insert("trigger".into(), serde_json::json!({"on": "manual"}));
        }
        Draft {
            name: name.to_string(),
            kind,
            description: String::new(),
            project: None,
            steps: Vec::new(),
            settings,
            status: Status::Draft,
            tasks: Vec::new(),
            proposal: Vec::new(),
        }
    }

    /// A workflow file's text as a draft: its steps become the step list,
    /// every other section is kept in `settings`. Comments are not kept.
    pub fn from_text(text: &str) -> Result<Draft> {
        let doc: Value = toml::from_str(text).context("parsing the workflow file")?;
        let Value::Object(mut top) = doc else {
            bail!("a workflow file is a table");
        };
        let name = top
            .remove("name")
            .and_then(|v| v.as_str().map(str::to_string))
            .context("the workflow has no name")?;
        let kind = match top.remove("kind").as_ref().and_then(Value::as_str) {
            Some("run") => WorkflowKind::Run,
            _ => WorkflowKind::Build,
        };
        let description = top
            .remove("description")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let steps = match top.remove("steps") {
            Some(v) => serde_json::from_value(v).context("reading the workflow's steps")?,
            None => Vec::new(),
        };
        Ok(Draft {
            name,
            kind,
            description,
            project: None,
            steps,
            settings: top,
            status: Status::Draft,
            tasks: Vec::new(),
            proposal: Vec::new(),
        })
    }

    pub fn from_workflow(wf: &Workflow) -> Result<Draft> {
        Draft::from_text(&wf.text)
    }

    /// The workflow file this draft renders to, and the line each step is
    /// on (one step per line). A placeholder step renders as the action it
    /// names: the file is what the catalog will hold once the action does.
    pub fn render(&self) -> Result<(String, Vec<usize>)> {
        let s = |v: &str| toml::Value::String(v.to_string()).to_string();
        let mut out = format!("name = {}\n", s(&self.name));
        if self.kind == WorkflowKind::Run {
            out.push_str("kind = \"run\"\n");
        }
        if !self.description.is_empty() {
            out.push_str(&format!("description = {}\n", s(&self.description)));
        }
        out.push_str("\nsteps = [\n");
        let mut lines = Vec::new();
        for st in &self.steps {
            lines.push(out.matches('\n').count() + 1);
            out.push_str(&format!("  {},\n", st.render()));
        }
        out.push_str("]\n");
        if !self.settings.is_empty() {
            let table = toml::Value::try_from(&self.settings).context("rendering the settings")?;
            out.push('\n');
            out.push_str(&toml::to_string(&table).context("rendering the settings")?);
        }
        Ok((out, lines))
    }

    /// The placeholder rule. Every step names an action; one that is not in
    /// the catalog must carry a placeholder whose inputs and outputs are
    /// written, and two steps naming the same missing action must agree on
    /// its contract (one task builds it). Returns each violation with its
    /// step.
    pub fn placeholder_problems(&self, known: &BTreeSet<String>) -> Vec<Problem> {
        let mut out = Vec::new();
        let mut seen: BTreeMap<&str, &Placeholder> = BTreeMap::new();
        let mut bad = |step: usize, message: String| {
            out.push(Problem {
                line: None,
                step: Some(step),
                message,
            })
        };
        for (i, st) in self.steps.iter().enumerate() {
            if st.action.is_empty() || known.contains(&st.action) {
                continue;
            }
            let Some(p) = &st.placeholder else {
                if st.extra.contains_key("workflow") {
                    continue;
                }
                bad(
                    i,
                    format!(
                        "step {} names action {:?}, which is not in the catalog; write its one-line contract (inputs, outputs, kind) to keep it as a placeholder",
                        i + 1,
                        st.action
                    ),
                );
                continue;
            };
            if !valid_name(&st.action) {
                bad(
                    i,
                    format!(
                        "placeholder {:?} is not a valid action name (lowercase letters, digits and dashes)",
                        st.action
                    ),
                );
            }
            if p.inputs.trim().is_empty() || p.outputs.trim().is_empty() {
                bad(
                    i,
                    format!(
                        "placeholder {:?} needs its inputs and its outputs written",
                        st.action
                    ),
                );
            }
            match seen.get(st.action.as_str()) {
                Some(first) if *first != p => bad(
                    i,
                    format!(
                        "step {} gives placeholder {:?} a different contract than an earlier step",
                        i + 1,
                        st.action
                    ),
                ),
                _ => {
                    seen.insert(&st.action, p);
                }
            }
        }
        out
    }

    /// Placeholders whose action is not in the catalog, one per action, in
    /// step order.
    pub fn pending(&self, known: &BTreeSet<String>) -> Vec<(String, Placeholder)> {
        let mut out: Vec<(String, Placeholder)> = Vec::new();
        for st in &self.steps {
            if let Some(p) = &st.placeholder
                && !known.contains(&st.action)
                && !out.iter().any(|(n, _)| *n == st.action)
            {
                out.push((st.action.clone(), p.clone()));
            }
        }
        out
    }

    /// Lint the draft: the catalog's own linter over its rendered file,
    /// given a stand-in for each pending placeholder, plus the placeholder
    /// rule. Writes nothing.
    pub fn check(&self, home: &Path) -> Result<Annotated> {
        let actions = known_actions(home)?;
        let known: BTreeSet<String> = actions.keys().cloned().collect();
        let (toml, lines) = self.render()?;
        let pending = self.pending(&known);
        let stand_ins = pending
            .iter()
            .map(|(n, p)| stand_in(n, p, self.kind))
            .collect::<Result<Vec<_>>>()?;
        let mut problems = self.placeholder_problems(&known);
        let placed = |p: LintProblem| Problem {
            step: p.line.and_then(|l| lines.iter().position(|x| *x == l)),
            line: p.line,
            message: p.message,
        };
        let placeholder_ok = problems.is_empty();
        for p in lint_with(home, Some(&self.name), &toml, stand_ins)? {
            // An unknown action without its contract is already reported
            // as a placeholder problem, once.
            if placeholder_ok || !p.message.contains("unknown action") {
                problems.push(placed(p));
            }
        }
        let info = self
            .steps
            .iter()
            .map(|st| step_info(st, &actions))
            .collect();
        Ok(Annotated {
            draft: self.clone(),
            toml,
            clean: problems.is_empty(),
            problems,
            info,
            pending: pending.into_iter().map(|(n, _)| n).collect(),
        })
    }
}

fn step_info(st: &DraftStep, actions: &BTreeMap<String, ActionDef>) -> StepInfo {
    match actions.get(&st.action) {
        Some(a) => StepInfo {
            kind: Some(a.kind),
            contract: Some(a.contract.as_str().to_string()),
            description: a.description.clone(),
            placeholder: st.placeholder.is_some(),
            landed: st.placeholder.is_some(),
        },
        None => StepInfo {
            kind: st.placeholder.as_ref().map(|p| p.kind),
            contract: None,
            description: st
                .placeholder
                .as_ref()
                .map(Placeholder::line)
                .unwrap_or_default(),
            placeholder: st.placeholder.is_some(),
            landed: false,
        },
    }
}

/// The action the linter is given for a placeholder: the declared kind,
/// the contract line as its description, and nothing else.
fn stand_in(name: &str, p: &Placeholder, wf: WorkflowKind) -> Result<ActionDef> {
    let desc = toml::Value::String(p.line()).to_string();
    let body = match (p.kind, wf) {
        (Kind::Operation, _) => "kind = \"operation\"\nrun = [\"true\"]\n".to_string(),
        (Kind::Directive, WorkflowKind::Run) => {
            "kind = \"directive\"\ncontract = \"plan\"\nschema = '{\"type\":\"object\"}'\n"
                .to_string()
        }
        (Kind::Directive, WorkflowKind::Build) => {
            "kind = \"directive\"\ncontract = \"code\"\n".to_string()
        }
    };
    let text = format!("name = {name:?}\ndescription = {desc}\n{body}");
    parse_action(Path::new(&format!("{name}.toml")), &text, String::new())
        .with_context(|| format!("the placeholder {name:?} as a stand-in action"))
}

fn path_of(home: &Path, name: &str) -> Result<PathBuf> {
    anyhow::ensure!(valid_name(name), "{name:?} is not a valid draft name");
    Ok(home.join(DIR).join(format!("{name}.json")))
}

/// Save a draft under `<home>/drafts/`, replacing any earlier one of the
/// name, atomically.
pub fn save(home: &Path, d: &Draft) -> Result<()> {
    let path = path_of(home, &d.name)?;
    std::fs::create_dir_all(path.parent().unwrap()).context("creating the drafts directory")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(d)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("saving {}", path.display()))
}

pub fn load(home: &Path, name: &str) -> Result<Option<Draft>> {
    let path = path_of(home, name)?;
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok(Some(
            serde_json::from_str(&t).with_context(|| format!("reading {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn remove(home: &Path, name: &str) -> Result<()> {
    match std::fs::remove_file(path_of(home, name)?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Every saved draft, by name.
pub fn list(home: &Path) -> Result<Vec<Draft>> {
    let dir = home.join(DIR);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok()?.path().file_stem()?.to_str().map(str::to_string))
        .filter(|n| valid_name(n))
        .collect();
    names.sort();
    names.dedup();
    let mut out = Vec::new();
    for n in names {
        out.extend(load(home, &n)?);
    }
    Ok(out)
}

/// One pass of "enable on its own": every `incomplete` draft whose
/// placeholders' actions have all landed in the catalog and that now lints
/// clean is written into the catalog, committed there, and marked
/// `enabled`. Returns the names enabled. A draft that cannot be checked
/// this pass is left as it is and reported on stderr; it does not stop the
/// others.
pub async fn reconcile(home: &Path) -> Result<Vec<String>> {
    let mut enabled = Vec::new();
    for mut d in list(home)? {
        if d.status != Status::Incomplete {
            continue;
        }
        let a = match d.check(home) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("draft {}: {e:#}", d.name);
                continue;
            }
        };
        if transition(d.status, a.pending.len(), a.clean) != Status::Enabled {
            continue;
        }
        commit_to_catalog(home, &d.name, &a.toml, &format!("enable draft {}", d.name)).await?;
        d.status = Status::Enabled;
        save(home, &d)?;
        enabled.push(d.name);
    }
    Ok(enabled)
}

/// Write `text` as `<name>.toml` in the operator's catalog and commit just
/// that file in the catalog's own git; the commit's hash (the current one
/// when the file did not change).
pub async fn commit_to_catalog(
    home: &Path,
    name: &str,
    text: &str,
    message: &str,
) -> Result<String> {
    let dir = super::catalog_dir(home)?;
    let file = format!("{name}.toml");
    std::fs::write(dir.join(&file), text)
        .with_context(|| format!("writing {file} into the catalog"))?;
    Ok(match crate::git::commit_path(&dir, &file, message).await? {
        Some(h) => h,
        None => crate::git::rev_parse(&dir, "HEAD").await?,
    })
}

#[cfg(test)]
mod tests;
