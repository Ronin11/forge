//! `forge workflows lint --stdin`: the catalog's own linter, in one place so
//! an editor (the draft editor, `workflows::draft`) and `put` run the
//! identical check.

use super::*;

/// One problem `forge workflows lint --stdin` found in a candidate
/// workflow file's text: the line the parser could place it at (a syntax
/// or shape error has one; a semantic error found only after a clean
/// parse — an unknown action, a data-flow violation — does not), and the
/// message. No `file`, unlike `ValidateProblem`: a candidate as typed has
/// none.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LintProblem {
    pub line: Option<usize>,
    pub message: String,
}

/// The catalog `forge workflows lint --stdin` checks a candidate against:
/// every built-in action, operation and workflow, parsed once in memory
/// from `BUILTIN_ACTIONS`/`BUILTIN_OPERATIONS`/`BUILTIN_WORKFLOWS`
/// (`builtin_actions_map` already does this for actions to avoid
/// `ensure`'s side effect), overlaid with the operator's own
/// `<home>/workflows/actions/*.toml` and `<home>/workflows/*.toml` when
/// `dir_of(home)` already exists. Never calls `ensure`: a fresh home lints
/// against the built-ins alone and gains no files from linting. A sibling
/// file that fails to parse is skipped rather than failing the whole
/// catalog, the same best-effort `lint` has always given a candidate.
fn lint_catalog(home: &Path) -> Result<(BTreeMap<String, Workflow>, BTreeMap<String, ActionDef>)> {
    let mut actions = builtin_actions_map()?;
    let mut workflows: BTreeMap<String, Workflow> = BTreeMap::new();
    for (file, text) in BUILTIN_WORKFLOWS {
        let wf = parse_workflow(Path::new(file), text, String::new())
            .with_context(|| format!("built-in {file}"))?;
        workflows.insert(wf.name.clone(), wf);
    }

    let dir = dir_of(home);
    if dir.exists() {
        for path in toml_files_if_present(&dir.join("actions"))? {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(a) = parse_action(&path, &text, String::new())
            {
                actions.insert(a.name.clone(), a);
            }
        }
        for path in toml_files_if_present(&dir)? {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(wf) = parse_workflow(&path, &text, String::new())
            {
                workflows.insert(wf.name.clone(), wf);
            }
        }
    }
    Ok((workflows, actions))
}

/// The line of the first still-unconsumed `<field> = "<name>"` in `text`
/// at or after `*after` (a byte offset), advancing `*after` past the
/// match so a second step naming the same action or workflow lands on its
/// own line rather than the first one's, repeated. `*after` starts at the
/// `steps` key so a name that also appears in, say, the description does
/// not steal the line.
fn step_ref_line(text: &str, field: &str, name: &str, after: &mut usize) -> Option<usize> {
    let needle = format!("{field} = {name:?}");
    let pos = text[*after..].find(&needle)? + *after;
    *after = pos + needle.len();
    Some(text[..pos].matches('\n').count() + 1)
}

/// The line of the `steps` key itself: where a whole-flow problem with no
/// span of its own (a data-flow violation from `check_flow`, a job-step
/// shape error from `job_steps`) is placed.
fn steps_key_line(text: &str) -> Option<usize> {
    let pos = text.find("steps")?;
    Some(text[..pos].matches('\n').count() + 1)
}

/// `forge workflows lint --stdin [--name <name>]`: parse a candidate
/// workflow file's text and check it resolves against the operator's own
/// catalog — every action or workflow reference it names known, the
/// data-flow rule holding, `[trigger]` well-formed for a run workflow —
/// without writing anything, not even to a fresh home, so an editor can
/// lint on every keystroke (docs/WORKFLOWS.md). `name` is the file name
/// the candidate would be saved under, used the way `parse_workflow` uses
/// a real file's stem (its own `name = "..."` must match); when absent,
/// the candidate's own declared name stands in for it, so a fresh draft
/// lints clean before the operator has chosen where to save it.
/// Every unknown action or workflow reference is reported, one problem
/// each with its own line, not just the first: `wf.steps` is walked by
/// hand before resolving. Only once every reference is known is the
/// candidate actually resolved (`job_steps` for a run workflow, else
/// `splice` and `check_flow`), and that result's error, if any, is
/// appended as a further problem — a whole-flow error with no span of its
/// own is placed at the `steps` key's line.
pub fn lint(home: &Path, name: Option<&str>, text: &str) -> Result<Vec<LintProblem>> {
    lint_with(home, name, text, Vec::new())
}

/// [`lint`], with `stand_ins` — actions that do not exist yet — taken as
/// known, so a draft that names a placeholder action still has the rest of
/// its flow (data flow, edges, run-workflow shape) checked against what the
/// placeholder promised. A real action of the same name wins.
pub fn lint_with(
    home: &Path,
    name: Option<&str>,
    text: &str,
    stand_ins: Vec<ActionDef>,
) -> Result<Vec<LintProblem>> {
    let (mut workflows, mut actions) = lint_catalog(home)?;
    for a in stand_ins {
        actions.entry(a.name.clone()).or_insert(a);
    }
    let stem = match name {
        Some(n) => n.to_string(),
        None => declared_name(text).unwrap_or_else(|| "candidate".to_string()),
    };
    let path = PathBuf::from(format!("{stem}.toml"));

    let wf = match parse_workflow(&path, text, String::new()) {
        Ok(wf) => wf,
        Err(e) => {
            return Ok(vec![LintProblem {
                line: error_line(&e, text),
                message: format!("{e:#}"),
            }]);
        }
    };

    workflows.insert(wf.name.clone(), wf.clone());

    let mut problems = Vec::new();
    let mut unknown = false;
    let mut after = text.find("steps").unwrap_or(0);
    for s in &wf.steps {
        if let Some(a) = &s.action {
            if !actions.contains_key(a) {
                problems.push(LintProblem {
                    line: step_ref_line(text, "action", a, &mut after),
                    message: format!("workflow {:?} references unknown action {:?}", wf.name, a),
                });
                unknown = true;
            }
        } else if let Some(w) = &s.workflow
            && !workflows.contains_key(w)
        {
            problems.push(LintProblem {
                line: step_ref_line(text, "workflow", w, &mut after),
                message: format!("workflow {:?} references unknown workflow {:?}", wf.name, w),
            });
            unknown = true;
        }
    }

    if !unknown {
        let result = if wf.kind == WorkflowKind::Run {
            job_steps(&wf, &workflows, &actions).map(|_| ())
        } else {
            let mut out = Resolved::default();
            splice(&wf, &workflows, &actions, &mut Vec::new(), &mut out)
                .and_then(|()| check_flow(&out.steps))
        };
        if let Err(e) = result {
            problems.push(LintProblem {
                line: error_line(&e, text).or_else(|| steps_key_line(text)),
                message: format!("{e:#}"),
            });
        }
    }

    Ok(problems)
}

/// Every action a candidate may name today: the built-ins overlaid with the
/// operator's own catalog, read the way [`lint`] reads it (no `ensure`, no
/// writes). What the draft editor's picker lists and what decides whether
/// a step's action still needs building.
pub fn known_actions(home: &Path) -> Result<BTreeMap<String, ActionDef>> {
    Ok(lint_catalog(home)?.1)
}
