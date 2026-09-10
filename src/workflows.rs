//! Workflows are data: one TOML file per workflow in `<FORGE2_HOME>/workflows/`,
//! an ordered list of agent steps from a closed set of kinds, each with
//! optional per-step parameters. The kernel verifies after every step and
//! pushes after the last; those are not steps because they are not
//! optional. A workflow's identity is its name plus a content hash, which
//! every task records, so two versions of "tdd" are never averaged together.
//! `check` is the one definition of a valid file; the engine refuses what
//! it rejects, doctor reports it, and any future tool calls it rather than
//! reimplementing it. Editing and committing the files is plain git.
//! See docs/WORKFLOWS.md.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// An agent writes the change in the task's clone.
    Code,
    /// An agent writes tests, only inside the verification namespace, that
    /// fail on the base commit; its summary becomes the coder's interface.
    Tests,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Step::Code => "code",
            Step::Tests => "tests",
        }
    }
    fn parse(s: &str) -> Option<Step> {
        match s {
            "code" => Some(Step::Code),
            "tests" => Some(Step::Tests),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct StepRaw {
    kind: String,
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
#[derive(Deserialize, serde::Serialize, Clone, Debug)]
pub struct Meta {
    /// When this workflow is the right choice.
    #[serde(default)]
    pub use_when: String,
    /// When it is the wrong choice.
    #[serde(default)]
    pub avoid_when: String,
    /// What the repository or task must provide (e.g. "namespace", "test check").
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

#[derive(Clone, Debug)]
pub struct StepDef {
    pub kind: Step,
    /// Overrides for this step; `None` means the task's own value.
    pub model: Option<String>,
    pub max_turns: Option<u32>,
    pub timeout_secs: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub steps: Vec<StepDef>,
    /// FNV-1a of the file's bytes, 16 hex chars: the version tasks record.
    pub hash: String,
    pub path: PathBuf,
    /// The file's exact text: recorded on each task so runs stay self-describing.
    pub text: String,
    pub meta: Meta,
}

impl Workflow {
    pub fn steps_text(&self) -> String {
        self.steps
            .iter()
            .map(|s| s.kind.as_str())
            .collect::<Vec<_>>()
            .join(" → ")
    }
    pub fn has(&self, kind: Step) -> bool {
        self.steps.iter().any(|s| s.kind == kind)
    }
}

const BUILTIN: &[(&str, &str)] = &[
    (
        "direct.toml",
        "name = \"direct\"\n\
description = \"one agent writes the change; the kernel verifies\"\n\
steps = [{ kind = \"code\" }]\n\
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
  { kind = \"tests\", max_turns = 40 },\n\
  { kind = \"code\" },\n\
]\n\
\n\
[meta]\n\
use_when = \"the task adds behavior that a test can pin down and the repo's checks would not otherwise catch a wrong implementation\"\n\
avoid_when = \"the task is a refactor, a rename, docs, or config; or the repo has no test check\"\n\
requires = [\"[verify] namespace in forge.toml\", \"a check named test\"]\n\
cost_factor = 2.5\n",
    ),
];

fn fnv1a(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn parse(path: &Path, text: &str) -> Result<Workflow> {
    let raw: WorkflowRaw =
        toml::from_str(text).with_context(|| format!("parsing {}", path.display()))?;
    if raw.steps.is_empty() {
        bail!("{}: a workflow needs at least one step", path.display());
    }
    let mut steps = Vec::new();
    for s in raw.steps {
        let kind = Step::parse(&s.kind).with_context(|| {
            format!(
                "{}: unknown step kind {:?} (kernel steps: code, tests)",
                path.display(),
                s.kind
            )
        })?;
        steps.push(StepDef {
            kind,
            model: s.model,
            max_turns: s.max_turns,
            timeout_secs: s.timeout_secs,
        });
    }
    Ok(Workflow {
        name: raw.name,
        description: raw.description,
        steps,
        hash: fnv1a(text.as_bytes()),
        path: path.to_path_buf(),
        text: text.to_string(),
        meta: raw.meta,
    })
}

/// Every workflow in `<home>/workflows/`, writing the built-ins first when
/// the directory is empty. Sorted by name.
/// The directory exists and, if it holds no workflow at all, the built-ins.
fn ensure_builtins(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let has_any = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .any(|e| e.path().extension().is_some_and(|x| x == "toml"));
    if !has_any {
        for (file, text) in BUILTIN {
            std::fs::write(dir.join(file), text)?;
        }
    }
    Ok(())
}

pub fn load_all(home: &Path) -> Result<Vec<Workflow>> {
    let dir = home.join("workflows");
    ensure_builtins(&dir)?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|x| x != "toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        out.push(parse(&path, &text)?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn get(home: &Path, name: &str) -> Result<Option<Workflow>> {
    Ok(load_all(home)?.into_iter().find(|w| w.name == name))
}

/// One thing wrong with a workflow file, and whether it blocks use.
#[derive(Debug, PartialEq, Eq)]
pub struct Problem {
    pub file: String,
    pub blocking: bool,
    pub what: String,
}

/// Every file in the directory, checked structurally. Parse errors are
/// reported as problems rather than returned as errors, so one bad file
/// does not hide the rest. Deterministic: same files, same list.
pub fn check(home: &Path) -> Result<Vec<Problem>> {
    let dir = home.join("workflows");
    ensure_builtins(&dir)?;
    let mut problems = Vec::new();
    let mut names: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.extension().is_none_or(|x| x != "toml") {
            continue;
        }
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path)?;
        let w = match parse(&path, &text) {
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
        if let Some(other) = names.insert(w.name.clone(), file.clone()) {
            problems.push(Problem {
                file: file.clone(),
                blocking: true,
                what: format!("duplicate name {:?}, also in {other}", w.name),
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
        if w.has(Step::Tests) && !w.meta.requires.iter().any(|r| r.contains("namespace")) {
            problems.push(Problem {
                file: file.clone(),
                blocking: false,
                what: "has a tests step but [meta] requires does not mention the namespace".into(),
            });
        }
        for st in &w.steps {
            if st.max_turns == Some(0) {
                problems.push(Problem {
                    file: file.clone(),
                    blocking: true,
                    what: format!("step {} has max_turns = 0", st.kind.as_str()),
                });
            }
            if st.timeout_secs == Some(0) {
                problems.push(Problem {
                    file: file.clone(),
                    blocking: true,
                    what: format!("step {} has timeout_secs = 0", st.kind.as_str()),
                });
            }
        }
    }
    Ok(problems)
}

/// Files changed since the last commit of the workflows directory, or all
/// files if it has never been committed.
pub fn uncommitted(home: &Path) -> Result<Vec<String>> {
    let dir = home.join("workflows");
    if !dir.join(".git").exists() {
        return Ok(std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect());
    }
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["status", "--porcelain"])
        .output()?;
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| l[3..].to_string())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_written_parsed_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let all = load_all(dir.path()).unwrap();
        let names: Vec<&str> = all.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["direct", "tdd"]);
        let tdd = get(dir.path(), "tdd").unwrap().unwrap();
        assert_eq!(tdd.steps_text(), "tests → code");
        assert_eq!(tdd.meta.cost_factor, 2.5);
        assert!(tdd.meta.requires.iter().any(|r| r.contains("namespace")));
        assert_eq!(
            get(dir.path(), "direct").unwrap().unwrap().meta.cost_factor,
            1.0
        );
        assert_eq!(tdd.steps[0].max_turns, Some(40));
        assert_eq!(tdd.hash.len(), 16);
        // Editing the file changes the version.
        std::fs::write(
            &tdd.path,
            std::fs::read_to_string(&tdd.path).unwrap() + "# tuned\n",
        )
        .unwrap();
        assert_ne!(get(dir.path(), "tdd").unwrap().unwrap().hash, tdd.hash);
        assert!(get(dir.path(), "nope").unwrap().is_none());
    }

    #[test]
    fn a_file_without_meta_costs_one_x() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("workflows/bare.toml"),
            "name = \"bare\"\nsteps = [{ kind = \"code\" }]\n",
        )
        .unwrap();
        let w = get(dir.path(), "bare").unwrap().unwrap();
        assert_eq!(w.meta.cost_factor, 1.0);
        assert!(w.meta.use_when.is_empty());
    }

    #[test]
    fn check_finds_structural_problems_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        assert!(
            check(dir.path()).unwrap().iter().all(|p| !p.blocking),
            "built-ins are clean"
        );
        let wf = dir.path().join("workflows");
        std::fs::write(
            wf.join("mismatch.toml"),
            "name = \"other\"\nsteps = [{ kind = \"code\", max_turns = 0 }]\n",
        )
        .unwrap();
        std::fs::write(
            wf.join("bad.toml"),
            "name = \"bad\"\nsteps = [{ kind = \"review\" }]\n",
        )
        .unwrap();
        std::fs::write(
            wf.join("direct2.toml"),
            "name = \"direct\"\nsteps = [{ kind = \"code\" }]\n[meta]\ncost_factor = 0\n",
        )
        .unwrap();
        let a = check(dir.path()).unwrap();
        let b = check(dir.path()).unwrap();
        assert_eq!(a, b, "deterministic");
        let blocking: Vec<&str> = a
            .iter()
            .filter(|p| p.blocking)
            .map(|p| p.what.as_str())
            .collect();
        assert!(
            blocking
                .iter()
                .any(|w| w.contains("does not match the file name")),
            "{blocking:?}"
        );
        assert!(
            blocking.iter().any(|w| w.contains("max_turns = 0")),
            "{blocking:?}"
        );
        assert!(
            blocking.iter().any(|w| w.contains("unknown step kind")),
            "{blocking:?}"
        );
        assert!(
            blocking.iter().any(|w| w.contains("duplicate name")),
            "{blocking:?}"
        );
        assert!(
            blocking
                .iter()
                .any(|w| w.contains("cost_factor must be positive")),
            "{blocking:?}"
        );
    }

    #[test]
    fn unknown_step_kinds_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("workflows/bad.toml"),
            "name = \"bad\"\nsteps = [{ kind = \"review\" }]\n",
        )
        .unwrap();
        let err = load_all(dir.path())
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_default();
        assert!(err.contains("unknown step kind"), "{err}");
    }
}
