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
pub const KERNEL_OPS: &[&str] = &["verify", "push", "integrate", "land", "clone"];

/// What an operation may produce. `branch`: it changes the tree, the
/// kernel commits the result and verifies it. `interface`: its stdout is
/// the interface the next code directive is shown. Everything else is a
/// directive's or the kernel's to produce.
pub const OPERATION_PRODUCES: &[&str] = &["branch", "interface"];

/// Contracts the kernel enforces for directives. A directive file names
/// one (default: its own name); any other value is rejected. Many
/// directives over few contracts (docs/ACTIONS.md).
pub const KNOWN_CONTRACTS: &[&str] = &["code", "tests", "review"];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Directive,
    Operation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Directive: which kernel contract runs it (default: the name).
    contract: Option<String>,
    /// Directive (code contract): paths the agent may change; a file, or a
    /// directory with a trailing slash, or a suffix like `*.md`.
    #[serde(default)]
    paths: Vec<String>,
    /// Directive: a short instruction appended to the task text.
    #[serde(default)]
    brief: String,
    /// Operation: run with the verification namespace overlaid from the
    /// trusted refs (a hidden suite the coder never sees).
    #[serde(default)]
    overlay: bool,
    /// Operation: its failure is the preceding directive's failure, fed
    /// back as a retry, rather than a one-shot task failure.
    #[serde(default)]
    verifies: bool,
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
    pub contract: String,
    pub paths: Vec<String>,
    pub brief: String,
    pub overlay: bool,
    pub verifies: bool,
    pub hash: String,
    pub text: String,
}

impl ActionDef {
    /// An operation that changes the tree: the kernel commits what it
    /// changed and verifies the result, as it does after a directive.
    pub fn mutates(&self) -> bool {
        self.kind == Kind::Operation && self.produces.iter().any(|p| p == "branch")
    }
    /// An operation whose stdout becomes the interface the coder is shown.
    pub fn yields_interface(&self) -> bool {
        self.kind == Kind::Operation && self.produces.iter().any(|p| p == "interface")
    }
    /// An operation that reads the task's hidden tests: it runs in a
    /// scratch copy of base with the verify ref overlaid, never in the
    /// coder's clone.
    pub fn reads_verify_ref(&self) -> bool {
        self.kind == Kind::Operation && self.consumes.iter().any(|c| c == "verify_ref")
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default)]
    pub use_when: String,
    #[serde(default)]
    pub avoid_when: String,
    #[serde(default)]
    pub requires: Vec<String>,
    /// Ignored. Costs are measured from runs, never declared; the field is
    /// accepted so older files still load, and `check` warns about it.
    #[serde(default, skip_serializing)]
    pub cost_factor: Option<f64>,
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
        "review.toml",
        "name = \"review\"\n\
kind = \"directive\"\n\
description = \"an independent session reads and runs the branch; it may not commit; it can only demote the task to human review, and only with something it executed\"\n\
consumes = [\"branch\", \"verdict\"]\n\
produces = [\"review\"]\n\
max_turns = 40\n",
    ),
    (
        "docs.toml",
        "name = \"docs\"\n\
kind = \"directive\"\n\
contract = \"code\"\n\
description = \"the code contract confined to documentation: only docs/ and Markdown files may change\"\n\
consumes = [\"branch\"]\n\
produces = [\"branch\"]\n\
paths = [\"docs/\", \"*.md\"]\n\
max_turns = 20\n",
    ),
    (
        "fix.toml",
        "name = \"fix\"\n\
kind = \"directive\"\n\
contract = \"code\"\n\
description = \"the code contract on a small, fast model with few turns, for tasks that are precisely specified and small\"\n\
consumes = [\"branch\"]\n\
produces = [\"branch\"]\n\
model = \"haiku\"\n\
max_turns = 15\n",
    ),
    (
        "polish.toml",
        "name = \"polish\"\n\
kind = \"directive\"\n\
contract = \"code\"\n\
description = \"a second pass over the branch told only to find and fix defects, never to add scope\"\n\
consumes = [\"branch\", \"verdict\"]\n\
produces = [\"branch\"]\n\
brief = \"The change for this task is already on the branch. Do not add features or scope. Read the diff against the base, run the checks, look for defects, missing edge cases, and untested paths, and fix what you find with tests. If you find nothing to fix, commit nothing and say so.\"\n\
max_turns = 20\n",
    ),
    (
        "document.toml",
        "name = \"document\"\n\
kind = \"directive\"\n\
contract = \"code\"\n\
description = \"a documentation pass over the branch: docs and code comments brought in line with the diff, nothing else\"\n\
consumes = [\"branch\"]\n\
produces = [\"branch\"]\n\
brief = \"The change for this task is already on the branch. Your only job is documentation. Read the diff of this branch against the base branch named above (`git diff <base>...HEAD`) and bring the documentation in line with it: README and docs/ where behavior, commands, configuration, or interfaces changed; doc comments on the functions, types, and modules the diff added or changed, in the style the file already uses. Do not change behavior, tests, or any code outside comments; an operation after you checks exactly that. If nothing needs documenting, commit nothing and say so.\"\n\
max_turns = 20\n",
    ),
    (
        "graph.toml",
        "name = \"graph\"\n\
kind = \"directive\"\n\
contract = \"code\"\n\
description = \"maintains docs/SYSTEM.md, the system map: components, what each owns, and the data flows between them, as a Mermaid graph plus prose\"\n\
consumes = [\"branch\"]\n\
produces = [\"branch\"]\n\
paths = [\"docs/SYSTEM.md\"]\n\
brief = \"Maintain docs/SYSTEM.md, the map of this system: its components (modules, services, stores, entry points, external systems), what each one owns, and the data that flows between them. The file is one Mermaid `graph` block naming the components and their flows, followed by one short paragraph per component. Read the diff of this branch against the base branch named above and the tree it touched; add, remove, or reword only what the change affected, and create the file from the whole tree if it does not exist. Name real paths in the tree, never invented ones; an operation after you checks every path. Nothing but docs/SYSTEM.md may change.\"\n\
max_turns = 20\n",
    ),
    (
        "playwright.toml",
        "name = \"playwright\"\n\
kind = \"operation\"\n\
description = \"run the hidden Playwright suite from forge-verify against the branch: the page must be playable; failure goes back to the coder\"\n\
consumes = [\"branch\"]\n\
produces = []\n\
run = [\"npx\", \"playwright\", \"test\", \"--config\", \"e2e/playwright.config.ts\"]\n\
overlay = true\n\
verifies = true\n\
timeout_secs = 900\n",
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

const BUILTIN_OPERATIONS: &[(&str, &str)] = &[
    (
        "comments-only.toml",
        r#"name = "comments-only"
kind = "operation"
description = "fails when the preceding step changed anything but comments and documentation: the guard behind the document directive. Comment syntax is recognised by line prefix (//, #, *, /*, */, <!--, -->, --, and triple quotes); docs/ and Markdown are free."
consumes = ["branch"]
verifies = true
run = ["bash", "-c", '''
set -e
from="${FORGE_PREV_SHA:-$FORGE_BASE_SHA}"
sq=$(printf "\x27")
bad=$(git diff --unified=0 "$from" HEAD -- . ':(exclude)docs/**' ':(exclude,glob)**/*.md' ':(exclude,glob)*.md' \
  | grep -E '^[+-]' | grep -vE '^(\+\+\+|---) ' | sed -E 's/^[+-]//' \
  | grep -vE "^[[:space:]]*(//|#|\*|/\*|\*/|<!--|-->|--|\"\"\"|$sq$sq$sq)" | grep -vE '^[[:space:]]*$' || true)
if [ -n "$bad" ]; then
  echo "the documentation pass changed more than comments and docs since $from:"
  echo "$bad" | head -20
  exit 1
fi
echo "only comments and docs changed since $from"
''', "comments-only"]
"#,
    ),
    (
        "graph-check.toml",
        r#"name = "graph-check"
kind = "operation"
description = "fails unless docs/SYSTEM.md exists, holds a Mermaid block, and names only paths that exist in the tree: the guard behind the graph directive"
consumes = ["branch"]
verifies = true
run = ["bash", "-c", '''
set -e
f=docs/SYSTEM.md
test -f "$f" || { echo "$f is missing"; exit 1; }
grep -qE '^```mermaid' "$f" || { echo "$f has no mermaid block"; exit 1; }
missing=$(grep -oE '\b[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)+' "$f" | grep -vE '^(https?:|[0-9])' | sed -E 's/[.,;:)]+$//' | sort -u \
  | while read -r p; do git ls-files --error-unmatch -- "$p" >/dev/null 2>&1 || test -d "$p" || echo "$p"; done)
if [ -n "$missing" ]; then
  echo "$f names paths that do not exist in the tree:"
  echo "$missing"
  exit 1
fi
echo "$f has a mermaid block and names only real paths"
''', "graph-check"]
"#,
    ),
    (
        "diff-size.toml",
        r#"name = "diff-size"
kind = "operation"
description = "fails when the change against base is larger than a cap on lines and files: the guard against scope creep and rewrites. The caps are the two numbers at the end of `run`."
consumes = ["branch"]
run = ["bash", "-c", '''
set -e
max_lines=$1
max_files=$2
lines=$(git diff --numstat "$FORGE_BASE_SHA" -- | awk '{ if ($1 != "-") a += $1 + $2 } END { print a + 0 }')
files=$(git diff --name-only "$FORGE_BASE_SHA" -- | wc -l)
echo "$files file(s), $lines line(s) changed against base (cap $max_files files, $max_lines lines)"
test "$lines" -le "$max_lines" && test "$files" -le "$max_files"
''', "diff-size", "800", "25"]
"#,
    ),
    (
        "fmt.toml",
        r#"name = "fmt"
kind = "operation"
description = "runs the tree's formatter and commits what it changed, so a formatting difference never costs a retry; the kernel verifies the result. Edit `run` for a repository whose formatter is not recognised."
consumes = ["branch"]
produces = ["branch"]
run = ["bash", "-c", '''
set -e
if [ -f Cargo.toml ]; then cargo fmt --all
elif [ -f go.mod ]; then gofmt -w .
elif [ -f pyproject.toml ] && command -v ruff >/dev/null; then ruff format .
elif [ -f package.json ] && [ -x node_modules/.bin/prettier ]; then node_modules/.bin/prettier --write . --log-level warn
else echo "no formatter recognised; nothing done"
fi
''']
"#,
    ),
    (
        "interface.toml",
        r#"name = "interface"
kind = "operation"
description = "the interface the hidden tests expect, extracted from the tests themselves rather than described by the agent that wrote them: the files, what they import, and the names they call, never their assertions. Runs in a scratch copy of base with the verify ref overlaid; the coder's clone never sees the tests. Replaces the tests directive's summary as the interface the coder is shown."
consumes = ["verify_ref"]
produces = ["interface"]
run = ["bash", "-c", '''
set -e
echo "Hidden tests, under $FORGE_NAMESPACE, judge this work. What they reference:"
for d in $FORGE_NAMESPACE; do find "$d" -type f 2>/dev/null; done | sort | while read -r f; do
  echo
  echo "== $f"
  grep -hE '^[[:space:]]*(use |import |from .+ import |require\(|#include|const .* = require)' "$f" | sed 's/^[[:space:]]*//' | sort -u || true
  grep -ohE '\b[A-Za-z_][A-Za-z0-9_]*\(' "$f" | sed 's/($//' \
    | grep -vxE 'if|for|while|switch|return|assert|print|println|printf|fn|function|def|expect|it|describe|test|catch|new' \
    | sort | uniq -c | sort -rn | awk '{ print "  calls " $2 " (" $1 "x)" }' || true
done
''']
"#,
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
",
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
",
    ),
    (
        "docs.toml",
        "name = \"docs\"\n\
description = \"documentation only: the agent may change docs/ and Markdown files, nothing else\"\n\
steps = [\n\
  { action = \"setup\" },\n\
  { action = \"docs\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task is documentation, a README, a changelog, or a design note\"\n\
avoid_when = \"any code has to change; the write scope will fail it\"\n\
requires = []\n\
",
    ),
    (
        "cheap.toml",
        "name = \"cheap\"\n\
description = \"a small fast model with few turns, for precisely specified small changes\"\n\
steps = [\n\
  { action = \"setup\" },\n\
  { action = \"fix\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task names the file and the change, and the repo's checks will catch a mistake\"\n\
avoid_when = \"the task needs design judgment or touches more than a couple of files\"\n\
requires = []\n\
",
    ),
    (
        "polish.toml",
        "name = \"polish\"\n\
description = \"the change, then a second pass that only finds and fixes defects\"\n\
steps = [\n\
  { action = \"setup\" },\n\
  { action = \"code\" },\n\
  { action = \"polish\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task is medium-sized and correctness matters more than cost\"\n\
avoid_when = \"the task is trivial; the second pass would only burn turns\"\n\
requires = []\n\
",
    ),
    (
        "reviewed.toml",
        "name = \"reviewed\"\n\
description = \"the change, then an independent reviewer that can only demote to human review with executed evidence\"\n\
steps = [\n\
  { action = \"setup\" },\n\
  { action = \"code\" },\n\
  { action = \"review\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task's correctness is not fully captured by tests and a second pair of eyes that runs the code is worth its cost\"\n\
avoid_when = \"the checks are strong and the task is small; the reviewer adds cost, not signal\"\n\
requires = []\n\
",
    ),
    (
        "playable.toml",
        "name = \"playable\"\n\
description = \"the change, then the hidden Playwright suite drives the built page; a failure goes back to the coder\"\n\
steps = [\n\
  { action = \"setup\" },\n\
  { action = \"code\" },\n\
  { action = \"playwright\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task touches anything a user sees or clicks; unit tests cannot tell whether a page works\"\n\
avoid_when = \"the repo has no e2e/ suite on forge-verify, or the change is pure simulation\"\n\
requires = [\"[verify] namespace including e2e/ in forge.toml\", \"a forge-verify branch with e2e/playwright.config.ts\", \"@playwright/test installed by setup\"]\n",
    ),
    (
        "documented.toml",
        "name = \"documented\"\n\
description = \"the change, then a documentation pass held to comments and docs\"\n\
steps = [\n\
  { workflow = \"direct\" },\n\
  { action = \"document\" },\n\
  { action = \"comments-only\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the change alters behavior, commands, configuration, or interfaces that the docs or doc comments describe\"\n\
avoid_when = \"the change is internal and the docs do not mention what it touches; the pass would commit nothing\"\n\
requires = []\n\
",
    ),
    (
        "mapped.toml",
        "name = \"mapped\"\n\
description = \"the change, then the system map in docs/SYSTEM.md brought in line with it\"\n\
steps = [\n\
  { workflow = \"direct\" },\n\
  { action = \"graph\" },\n\
  { action = \"graph-check\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the change adds, removes, or rewires a component or a data flow\"\n\
avoid_when = \"the change stays inside one component; the map would not move\"\n\
requires = []\n\
",
    ),
    (
        "tdd-reviewed.toml",
        "name = \"tdd-reviewed\"\n\
description = \"hidden tests first, the change, then an independent reviewer\"\n\
steps = [\n\
  { workflow = \"tdd\" },\n\
  { action = \"review\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task is important enough for both a hidden specification and a reviewer\"\n\
avoid_when = \"cost matters; this is the most expensive built-in\"\n\
requires = [\"[verify] namespace in forge.toml\", \"a check named test\"]\n\
",
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
    // Built-ins are written when missing and never overwritten: an existing
    // install gains new built-ins on upgrade and keeps its own edits.
    for (file, text) in BUILTIN_WORKFLOWS {
        let p = dir.join(file);
        if !p.exists() {
            std::fs::write(&p, text)?;
        }
    }
    for (file, text) in BUILTIN_ACTIONS.iter().chain(BUILTIN_OPERATIONS) {
        let p = actions.join(file);
        if !p.exists() {
            std::fs::write(&p, text)?;
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
            let contract = raw.contract.clone().unwrap_or_else(|| raw.name.clone());
            if !KNOWN_CONTRACTS.contains(&contract.as_str()) {
                bail!(
                    "{}: directive contract {:?} is not one the kernel enforces (known: {})",
                    path.display(),
                    contract,
                    KNOWN_CONTRACTS.join(", ")
                );
            }
            if !raw.paths.is_empty() && contract != "code" {
                bail!(
                    "{}: `paths` applies to the code contract only",
                    path.display()
                );
            }
        }
    }
    if raw.kind == Kind::Operation
        && (raw.contract.is_some() || !raw.paths.is_empty() || !raw.brief.is_empty())
    {
        bail!(
            "{}: contract, paths, and brief apply to directives only",
            path.display()
        );
    }
    if raw.kind == Kind::Directive && (raw.overlay || raw.verifies) {
        bail!(
            "{}: overlay and verifies apply to operations only",
            path.display()
        );
    }
    if raw.kind == Kind::Operation
        && let Some(p) = raw
            .produces
            .iter()
            .find(|p| !OPERATION_PRODUCES.contains(&p.as_str()))
    {
        bail!(
            "{}: an operation cannot produce {:?}; it may produce {}",
            path.display(),
            p,
            OPERATION_PRODUCES.join(", ")
        );
    }
    if raw.kind == Kind::Operation && raw.model.is_some() {
        bail!("{}: `model` applies to directives only", path.display());
    }
    let contract = raw.contract.clone().unwrap_or_else(|| raw.name.clone());
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
        contract,
        paths: raw.paths,
        brief: raw.brief,
        overlay: raw.overlay,
        verifies: raw.verifies,
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
        if s.action.kind == Kind::Directive || s.action.mutates() {
            have.insert("verdict");
            if s.action.contract == "review" {
                have.insert("review");
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
        if w.meta.cost_factor.is_some() {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "[meta] cost_factor is ignored: costs are measured from runs, never declared; remove it".into(),
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
            vec![
                "cheap",
                "direct",
                "docs",
                "documented",
                "mapped",
                "playable",
                "polish",
                "reviewed",
                "tdd",
                "tdd-reviewed"
            ]
        );
        for w in &all {
            resolve(dir.path(), &w.name).unwrap_or_else(|e| panic!("{}: {e:#}", w.name));
        }
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
        let rr = resolve(dir.path(), "tdd-reviewed").unwrap();
        assert_eq!(
            rr.steps
                .iter()
                .map(|s| s.action.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tests", "setup", "code", "review"]
        );
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
    fn directives_name_a_kernel_contract() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "actions/audit.toml",
            "name = \"audit\"\nkind = \"directive\"\ndescription = \"d\"\nconsumes = [\"branch\"]\n",
        );
        let problems = check(dir.path()).unwrap();
        assert!(
            problems
                .iter()
                .any(|p| p.blocking && p.what.contains("not one the kernel enforces")),
            "{problems:?}"
        );
        std::fs::remove_file(dir.path().join("workflows/actions/audit.toml")).unwrap();
        write(
            dir.path(),
            "actions/tidy.toml",
            "name = \"tidy\"\nkind = \"directive\"\ncontract = \"code\"\ndescription = \"d\"\nconsumes = [\"branch\"]\nproduces = [\"branch\"]\npaths = [\"src/\"]\nbrief = \"only tidy\"\n",
        );
        let a = load_actions(dir.path()).unwrap();
        assert_eq!(a["tidy"].contract, "code");
        assert_eq!(a["docs"].paths, vec!["docs/", "*.md"]);
        assert_eq!(a["polish"].contract, "code");
        assert!(!a["polish"].brief.is_empty());
        write(
            dir.path(),
            "actions/badpaths.toml",
            "name = \"badpaths\"\nkind = \"directive\"\ncontract = \"review\"\npaths = [\"x\"]\n",
        );
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|p| p.what.contains("code contract only"))
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
            vec!["setup", "code", "setup", "code"]
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
