//! Failure edges on run workflows (docs/EXECUTION.md, "Outcomes, then
//! edges"): a node id per step, the `on` entries resolved to node ids, and
//! the lint that refuses an edge which would skip a verification.

use super::{Kind, RunStep};
use anyhow::{Result, bail};

pub const FAILURE: &str = "failure";
pub const END: &str = "end";
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

pub fn node_id(index: usize, action: &str) -> String {
    format!("{index}-{action}")
}

fn target_index(workflow: &str, steps: &[RunStep], from: usize, to: &str) -> Result<usize> {
    if let Some(i) = steps.iter().position(|s| s.node == to) {
        return Ok(i);
    }
    let named: Vec<usize> = (0..steps.len())
        .filter(|&i| steps[i].action.name == to)
        .collect();
    match named.as_slice() {
        [i] => Ok(*i),
        [] => bail!(
            "{workflow:?}: step {:?} routes to {to:?}, which is neither a step, a node id nor `end`",
            steps[from].action.name
        ),
        _ => bail!(
            "{workflow:?}: step {:?} routes to {to:?}, which names more than one step; use a node id ({})",
            steps[from].action.name,
            named
                .iter()
                .map(|&i| steps[i].node.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Give every step its node id and rewrite each `on` target to a node id
/// (or `end`), refusing unknown keys and targets, and a forward edge that
/// would skip an operation that verifies.
pub(super) fn resolve(workflow: &str, steps: &mut [RunStep]) -> Result<()> {
    for (i, s) in steps.iter_mut().enumerate() {
        s.node = node_id(i, &s.action.name);
        if s.max_attempts == 0 {
            bail!(
                "{workflow:?}: step {:?} has max_attempts = 0",
                s.action.name
            );
        }
    }
    for from in 0..steps.len() {
        let edges = steps[from].on.clone();
        for (key, to) in edges {
            let s = &steps[from];
            if key != FAILURE && !s.action.outcomes.contains(&key) {
                bail!(
                    "{workflow:?}: step {:?} has an `on` entry for {key:?}, which is neither `failure` nor one of its action's outcomes ({})",
                    s.action.name,
                    s.action.outcomes.join(", ")
                );
            }
            let (node, first_kept) = if to == END {
                (END.to_string(), steps.len())
            } else {
                let j = target_index(workflow, steps, from, &to)?;
                (steps[j].node.clone(), j)
            };
            if let Some(skipped) = (from + 1..first_kept)
                .map(|k| &steps[k])
                .find(|k| k.action.kind == Kind::Operation && k.action.verifies)
            {
                bail!(
                    "{workflow:?}: step {:?} routes {key:?} to {to:?}, which would skip {:?}, a step that verifies; an edge cannot route around the judging",
                    steps[from].action.name,
                    skipped.action.name
                );
            }
            steps[from].on.insert(key, node);
        }
    }
    Ok(())
}
