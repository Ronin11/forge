//! The line grammar of `forge workflows draft NAME`: one command per line
//! on stdin, each an edit of the draft as data. Editing is pure — no
//! catalog, no store — so the session's I/O (linting after every change,
//! suggesting, saving, putting) stays with the caller, told what to do by
//! the [`Next`] an edit returns.

use crate::workflows::draft::{Draft, DraftStep, Placeholder};
use crate::workflows::{Kind, WorkflowKind};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

/// What the session does after an edit.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Next {
    /// The draft changed: lint it and show it.
    Edited,
    Show,
    Actions(Option<String>),
    Suggest(String),
    Save,
    Put {
        message: String,
        repo: Option<PathBuf>,
    },
    Help,
    Quit,
}

pub(super) const HELP: &str = "\
add ACTION [role=R] [judgment=J] [effect=E] [at=N]   add a catalog action
add NAME kind=operation|directive inputs=I outputs=O  add an action that does not exist yet (a placeholder)
set N key=value ...      change role, judgment, effect, action, or a placeholder's kind, inputs, outputs (empty clears)
edge N KEY TARGET        on failure (or an outcome) of step N go to a step, a node id, or end
unedge N KEY             remove that edge
rm N | mv N M            remove or move a step
describe TEXT | project NAME | kind build|run
actions [FILTER]         the catalog's actions with their contracts
suggest DESCRIPTION      one call to the author directive; its steps are proposed, not applied
accept [N ...]           append the proposed steps (all, or the numbered ones) to the draft
show | save | quit
put MESSAGE [repo=PATH]  commit to the catalog, or file a repository task; a placeholder files one build task each";

/// Split a line on whitespace; double quotes group, and are dropped, so
/// `inputs="a tree"` is one token `inputs=a tree`.
pub(super) fn tokens(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

fn index(draft: &Draft, tok: Option<&String>) -> Result<usize> {
    let n: usize = tok
        .context("which step? give its number")?
        .parse()
        .context("a step is named by its number")?;
    anyhow::ensure!(
        (1..=draft.steps.len()).contains(&n),
        "there is no step {n}; the draft has {}",
        draft.steps.len()
    );
    Ok(n - 1)
}

fn kind_of(v: &str) -> Result<Kind> {
    match v {
        "operation" => Ok(Kind::Operation),
        "directive" => Ok(Kind::Directive),
        _ => bail!("kind is operation or directive, not {v:?}"),
    }
}

/// Apply `key=value` to a step. An empty value clears the field.
fn set_field(step: &mut DraftStep, key: &str, value: &str) -> Result<()> {
    let opt = || (!value.is_empty()).then(|| value.to_string());
    fn ph(step: &mut DraftStep) -> &mut Placeholder {
        step.placeholder.get_or_insert(Placeholder {
            kind: Kind::Operation,
            inputs: String::new(),
            outputs: String::new(),
        })
    }
    match key {
        "role" => step.role = opt(),
        "judgment" => step.judgment = opt(),
        "effect" => step.effect = opt(),
        "action" => step.action = value.to_string(),
        "kind" => ph(step).kind = kind_of(value)?,
        "inputs" => ph(step).inputs = value.to_string(),
        "outputs" => ph(step).outputs = value.to_string(),
        _ => bail!(
            "{key:?} is not a step field (role, judgment, effect, action, kind, inputs, outputs)"
        ),
    }
    Ok(())
}

fn options(toks: &[String]) -> Result<Vec<(&str, &str)>> {
    toks.iter()
        .map(|t| {
            t.split_once('=')
                .with_context(|| format!("expected key=value, got {t:?}"))
        })
        .collect()
}

fn add(d: &mut Draft, toks: &[String]) -> Result<()> {
    let name = toks.get(1).context("add which action?")?;
    let mut step = DraftStep::named(name);
    let mut at = None;
    for (k, v) in options(&toks[2..])? {
        if k == "at" {
            at = Some(v.parse::<usize>().context("at= is a step number")?);
        } else {
            set_field(&mut step, k, v)?;
        }
    }
    let given = ["kind=", "inputs=", "outputs="]
        .iter()
        .filter(|p| toks[2..].iter().any(|t| t.starts_with(**p)))
        .count();
    anyhow::ensure!(
        given == 0 || given == 3,
        "a placeholder needs kind, inputs and outputs together"
    );
    let at = at.map_or(d.steps.len(), |n| n.saturating_sub(1).min(d.steps.len()));
    d.steps.insert(at, step);
    Ok(())
}

fn set_kind(d: &mut Draft, v: &str) -> Result<()> {
    match v {
        "build" => {
            d.kind = WorkflowKind::Build;
            for k in ["trigger", "limits", "assert", "skip_if", "env"] {
                d.settings.remove(k);
            }
        }
        "run" => {
            d.kind = WorkflowKind::Run;
            d.settings
                .entry("trigger")
                .or_insert_with(|| serde_json::json!({"on": "manual"}));
        }
        _ => bail!("kind is build or run, not {v:?}"),
    }
    Ok(())
}

fn accept(d: &mut Draft, toks: &[String]) -> Result<()> {
    anyhow::ensure!(
        !d.proposal.is_empty(),
        "nothing is proposed; `suggest` first"
    );
    let picked: Vec<DraftStep> = if toks.len() > 1 {
        toks[1..]
            .iter()
            .map(|t| {
                let n: usize = t
                    .parse()
                    .context("a proposed step is named by its number")?;
                d.proposal
                    .get(n.wrapping_sub(1))
                    .cloned()
                    .with_context(|| format!("there is no proposed step {n}"))
            })
            .collect::<Result<_>>()?
    } else {
        d.proposal.clone()
    };
    d.steps.extend(picked);
    d.proposal.clear();
    Ok(())
}

/// Apply one line to the draft.
pub(super) fn apply(d: &mut Draft, line: &str) -> Result<Next> {
    let toks = tokens(line);
    let Some(cmd) = toks.first().map(String::as_str) else {
        return Ok(Next::Show);
    };
    let rest = || line.trim_start()[cmd.len()..].trim().to_string();
    match cmd {
        "add" => add(d, &toks)?,
        "set" => {
            let i = index(d, toks.get(1))?;
            for (k, v) in options(&toks[2..])? {
                set_field(&mut d.steps[i], k, v)?;
            }
        }
        "edge" => {
            let i = index(d, toks.get(1))?;
            let (Some(k), Some(to)) = (toks.get(2), toks.get(3)) else {
                bail!("edge N KEY TARGET");
            };
            d.steps[i].on.insert(k.clone(), to.clone());
        }
        "unedge" => {
            let i = index(d, toks.get(1))?;
            let k = toks.get(2).context("unedge N KEY")?;
            d.steps[i].on.remove(k);
        }
        "rm" => {
            let i = index(d, toks.get(1))?;
            d.steps.remove(i);
        }
        "mv" => {
            let (from, to) = (index(d, toks.get(1))?, index(d, toks.get(2))?);
            let s = d.steps.remove(from);
            d.steps.insert(to, s);
        }
        "describe" => d.description = rest(),
        "project" => d.project = Some(rest()).filter(|p| !p.is_empty()),
        "kind" => set_kind(d, toks.get(1).map_or("", String::as_str))?,
        "accept" => accept(d, &toks)?,
        "show" | "lint" => return Ok(Next::Show),
        "actions" => return Ok(Next::Actions(toks.get(1).cloned())),
        "suggest" => return Ok(Next::Suggest(rest())),
        "save" => return Ok(Next::Save),
        "put" => return put(&toks),
        "help" | "?" => return Ok(Next::Help),
        "quit" | "exit" => return Ok(Next::Quit),
        _ => bail!("unknown command {cmd:?}; `help` lists them"),
    }
    Ok(Next::Edited)
}

fn put(toks: &[String]) -> Result<Next> {
    let mut repo = None;
    let mut words = Vec::new();
    for t in &toks[1..] {
        match t.strip_prefix("repo=") {
            Some(p) => repo = Some(PathBuf::from(p)),
            None => words.push(t.as_str()),
        }
    }
    let message = words.join(" ");
    anyhow::ensure!(!message.trim().is_empty(), "put needs a commit message");
    Ok(Next::Put { message, repo })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(d: &mut Draft, lines: &[&str]) {
        for l in lines {
            apply(d, l).unwrap_or_else(|e| panic!("{l}: {e:#}"));
        }
    }

    #[test]
    fn quotes_group_and_are_dropped() {
        assert_eq!(
            tokens(r#"add x inputs="a tree" outputs=b  "#),
            ["add", "x", "inputs=a tree", "outputs=b"]
        );
        assert_eq!(tokens(r#"set 1 judgment="""#), ["set", "1", "judgment="]);
    }

    #[test]
    fn a_two_step_draft_with_a_placeholder_is_built_line_by_line() {
        let mut d = Draft::new("w", WorkflowKind::Build);
        run(
            &mut d,
            &[
                "describe lint the docs",
                "add code",
                r#"add lint-docs kind=operation inputs="the tree" outputs="a verdict""#,
                "edge 2 failure end",
            ],
        );
        assert_eq!(d.description, "lint the docs");
        assert_eq!(d.steps.len(), 2);
        let ph = d.steps[1].placeholder.as_ref().unwrap();
        assert_eq!((ph.inputs.as_str(), ph.kind), ("the tree", Kind::Operation));
        assert_eq!(d.steps[1].on["failure"], "end");
        run(&mut d, &["unedge 2 failure", "mv 2 1", "rm 2"]);
        assert_eq!(d.steps[0].action, "lint-docs");
        assert_eq!(d.steps.len(), 1);
    }

    #[test]
    fn a_partial_placeholder_and_a_bad_step_number_are_refused() {
        let mut d = Draft::new("w", WorkflowKind::Build);
        assert!(apply(&mut d, "add x kind=operation").is_err());
        assert!(apply(&mut d, "rm 1").is_err());
        assert!(apply(&mut d, "frobnicate").is_err());
    }

    #[test]
    fn switching_kind_carries_the_sections_that_kind_needs() {
        let mut d = Draft::new("w", WorkflowKind::Build);
        run(&mut d, &["kind run"]);
        assert!(d.settings.contains_key("trigger"));
        run(&mut d, &["kind build"]);
        assert!(d.settings.is_empty());
    }

    #[test]
    fn accept_appends_the_proposed_steps_the_operator_picked() {
        let mut d = Draft::new("w", WorkflowKind::Build);
        d.proposal = vec![DraftStep::named("a"), DraftStep::named("b")];
        run(&mut d, &["accept 2"]);
        assert_eq!(d.steps.len(), 1);
        assert_eq!(d.steps[0].action, "b");
        assert!(d.proposal.is_empty());
    }

    #[test]
    fn put_takes_a_message_and_an_optional_repo() {
        let mut d = Draft::new("w", WorkflowKind::Build);
        assert_eq!(
            apply(&mut d, "put add the thing repo=/r").unwrap(),
            Next::Put {
                message: "add the thing".into(),
                repo: Some("/r".into())
            }
        );
        assert!(apply(&mut d, "put").is_err());
    }
}
