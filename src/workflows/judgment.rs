//! What an action declares for a judging (`jev`) provider: outcomes with
//! descriptions, typed questions, and confidence floors (docs/EXECUTION.md,
//! "The judgment tier").

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The two spellings of an action's `outcomes`.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum OutcomesRaw {
    List(Vec<String>),
    Described(BTreeMap<String, String>),
}

/// One typed question a `jev` directive asks besides its outcomes: `choice`,
/// `noul` or `score`, with the instructions it is judged by and its criteria
/// (a table of option = description, or a list of levels).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub criteria: Option<serde_json::Value>,
}

/// The name the outcomes' own question answers under, so no `[[questions]]`
/// entry may take it.
pub const OUTCOME_QUESTION: &str = "outcome";

pub(super) struct Parsed {
    /// Declared outcomes, then the `confidence_below` names.
    pub outcomes: Vec<String>,
    pub outcome_criteria: BTreeMap<String, String>,
    /// Ascending by threshold.
    pub confidence_below: Vec<(f64, String)>,
}

/// Validates an action's outcomes, questions and confidence floors.
pub(super) fn parse(
    path: &Path,
    operation: bool,
    raw_outcomes: &Option<OutcomesRaw>,
    questions: &[Question],
    raw_floors: &Option<serde_json::Value>,
) -> Result<Parsed> {
    let (mut outcomes, outcome_criteria): (Vec<String>, BTreeMap<String, String>) =
        match raw_outcomes {
            None => (Vec::new(), BTreeMap::new()),
            Some(OutcomesRaw::List(names)) => (
                names.clone(),
                names.iter().map(|n| (n.clone(), n.clone())).collect(),
            ),
            Some(OutcomesRaw::Described(m)) => (m.keys().cloned().collect(), m.clone()),
        };
    if raw_outcomes.is_some() {
        let mut seen = BTreeSet::new();
        if outcomes.is_empty()
            || outcomes
                .iter()
                .any(|n| n.trim().is_empty() || !seen.insert(n))
        {
            bail!(
                "{}: `outcomes` needs at least one name, none empty or repeated",
                path.display()
            );
        }
    }
    if operation && (!questions.is_empty() || raw_floors.is_some()) {
        bail!(
            "{}: questions and confidence_below apply to directives only",
            path.display()
        );
    }
    check_questions(path, questions)?;
    let mut floors = Vec::new();
    let mut leaves = Vec::new();
    if let Some(v) = raw_floors {
        flatten(path, v, String::new(), &mut leaves)?;
    }
    for (threshold, name) in &leaves {
        let t: f64 = threshold
            .parse()
            .ok()
            .filter(|t| *t > 0.0 && *t <= 1.0)
            .with_context(|| {
                format!(
                    "{}: `confidence_below` key {threshold:?} is not a number in (0, 1]",
                    path.display()
                )
            })?;
        if name.trim().is_empty() || outcomes.contains(name) {
            bail!(
                "{}: `confidence_below` outcome {name:?} is empty or already an outcome",
                path.display()
            );
        }
        floors.push((t, name.clone()));
    }
    if !floors.is_empty() && outcome_criteria.is_empty() {
        bail!(
            "{}: `confidence_below` needs `outcomes` to take the confidence of",
            path.display()
        );
    }
    floors.sort_by(|a, b| a.0.total_cmp(&b.0));
    outcomes.extend(floors.iter().map(|(_, n)| n.clone()));
    Ok(Parsed {
        outcomes,
        outcome_criteria,
        confidence_below: floors,
    })
}

/// `{ 0.6 = "uncertain" }` is TOML for `{ "0": { "6": "uncertain" } }`, a
/// dotted key, and `{ "0.6" = "uncertain" }` is the same threshold spelled
/// quoted: both flatten to the leaf `("0.6", "uncertain")`.
fn flatten(
    path: &Path,
    v: &serde_json::Value,
    key: String,
    out: &mut Vec<(String, String)>,
) -> Result<()> {
    match v {
        serde_json::Value::String(name) if !key.is_empty() => out.push((key, name.clone())),
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                let key = if key.is_empty() {
                    k.clone()
                } else {
                    format!("{key}.{k}")
                };
                flatten(path, v, key, out)?;
            }
        }
        _ => bail!(
            "{}: `confidence_below` is a table of threshold = \"outcome\"",
            path.display()
        ),
    }
    Ok(())
}

fn check_questions(path: &Path, questions: &[Question]) -> Result<()> {
    let mut names = BTreeSet::new();
    for q in questions {
        if q.name.trim().is_empty() || q.name == OUTCOME_QUESTION || !names.insert(&q.name) {
            bail!(
                "{}: a question needs a name, not empty, not {OUTCOME_QUESTION:?}, not repeated",
                path.display()
            );
        }
        if !["choice", "noul", "score"].contains(&q.kind.as_str()) {
            bail!(
                "{}: question {:?} has type {:?}; expected choice, noul or score",
                path.display(),
                q.name,
                q.kind
            );
        }
        if q.kind == "choice" && !q.criteria.as_ref().is_some_and(|c| c.is_object()) {
            bail!(
                "{}: choice question {:?} needs `criteria`, a table of option = description",
                path.display(),
                q.name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::parse_action;
    use std::path::Path;

    fn action(extra: &str) -> anyhow::Result<super::super::ActionDef> {
        let text = format!(
            "name = \"triage\"\nkind = \"directive\"\ncontract = \"plan\"\ndescription = \"Route it.\"\n{extra}"
        );
        parse_action(Path::new("triage.toml"), &text, "h".into())
    }

    #[test]
    fn outcomes_take_a_list_or_a_table_of_descriptions() {
        let a = action("outcomes = [\"reply\", \"ignore\"]").unwrap();
        assert_eq!(a.outcomes, ["reply", "ignore"]);
        assert_eq!(a.outcome_criteria["reply"], "reply");
        let b =
            action("[outcomes]\nreply = \"a person needs an answer\"\nignore = \"noise\"").unwrap();
        assert_eq!(b.outcomes, ["ignore", "reply"]);
        assert_eq!(b.outcome_criteria["reply"], "a person needs an answer");
    }

    #[test]
    fn a_confidence_floor_is_an_outcome_an_edge_may_route_on() {
        let a = action(
            "outcomes = [\"reply\"]\nconfidence_below = { 0.8 = \"check\", \"0.6\" = \"uncertain\" }",
        )
        .unwrap();
        assert_eq!(a.outcomes, ["reply", "uncertain", "check"]);
        assert_eq!(a.confidence_below[0], (0.6, "uncertain".to_string()));
        assert!(!a.outcome_criteria.contains_key("uncertain"));
        assert!(action("outcomes = [\"a\"]\nconfidence_below = { 2 = \"x\" }").is_err());
        assert!(action("outcomes = [\"a\"]\nconfidence_below = { 0.5 = \"a\" }").is_err());
        assert!(action("confidence_below = { 0.5 = \"x\" }").is_err());
    }

    #[test]
    fn questions_are_typed_and_cannot_shadow_the_outcomes() {
        let ok = action(
            "outcomes = [\"a\"]\n[[questions]]\nname = \"urgency\"\ntype = \"score\"\ncriteria = [\"low\", \"high\"]",
        )
        .unwrap();
        assert_eq!(ok.questions[0].kind, "score");
        let bad = |q: &str| action(&format!("outcomes = [\"a\"]\n[[questions]]\n{q}"));
        assert!(bad("name = \"outcome\"\ntype = \"noul\"").is_err());
        assert!(bad("name = \"x\"\ntype = \"essay\"").is_err());
        assert!(bad("name = \"x\"\ntype = \"choice\"").is_err());
    }
}
