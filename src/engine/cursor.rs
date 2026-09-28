//! The run cursor: where a task's run stands between workers. It is stored
//! (`tasks.run_json`), not held in memory, so a requeue, an orphaned claim
//! or a crash resumes at the step that was interrupted instead of at step
//! zero. It names the resolved workflow it was written against; a cursor
//! for a different resolution is not trusted.

use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunCursor {
    /// Hash of the resolved steps the cursor indexes into.
    pub workflow_hash: String,
    /// The step to run next (or, when it equals the step count, landing).
    pub idx: usize,
    /// The attempt number the run had reached.
    pub attempt: i64,
    /// Feedback a step owes a directive that has not run on it yet.
    #[serde(default)]
    pub owed: BTreeMap<i64, String>,
    /// What the tests step handed the later steps.
    #[serde(default)]
    pub interface: String,
    /// What the plan step handed the later steps.
    #[serde(default)]
    pub plan: String,
}

/// Where a claimed task's run starts.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// Nothing stored: a first run.
    Fresh,
    /// A stored cursor for this resolution: resume here.
    At(RunCursor),
    /// A stored cursor that cannot be used; start at zero and say why.
    Restart(String),
}

pub(crate) fn workflow_hash(actions_json: &str) -> String {
    Sha256::digest(actions_json.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl RunCursor {
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub(crate) fn from_json(raw: &str) -> Option<Self> {
        serde_json::from_str(raw).ok()
    }

    /// The cursor once the step at `self.idx` has verified.
    pub(crate) fn after(&self, attempt: i64) -> Self {
        Self {
            idx: self.idx + 1,
            attempt,
            ..self.clone()
        }
    }
}

/// Where a claim's run starts, read from the stored cursor.
pub(super) struct Resumed {
    pub hash: String,
    pub idx: usize,
    pub owed: HashMap<i64, String>,
    pub note: Option<String>,
}

/// Read the task's stored cursor against its resolution. A cursor is only
/// trusted in the worktree it was written for; what the earlier steps handed
/// the later ones comes back onto the task.
pub(super) fn resume(
    f: &Forge,
    t: &mut Task,
    resolved: &workflows::Resolved,
    had_worktree: bool,
) -> Result<Resumed, Fault> {
    let hash = workflow_hash(&t.actions_json);
    let stored = if had_worktree {
        f.store.run_cursor(t.id).env()?
    } else {
        None
    };
    let mut out = Resumed {
        hash: hash.clone(),
        idx: 0,
        owed: HashMap::new(),
        note: None,
    };
    let steps = resolved.steps.len();
    match start_from(stored.as_deref(), &hash, steps) {
        Start::Fresh => {}
        Start::Restart(why) => out.note = Some(format!("cursor   {why}; starting from step 1")),
        Start::At(c) => {
            if t.interface.is_empty() {
                t.interface = c.interface;
            }
            if t.plan.is_empty() {
                t.plan = c.plan;
            }
            out.idx = c.idx;
            out.owed = c.owed.into_iter().collect();
            out.note = Some(match resolved.steps.get(c.idx) {
                Some(s) => format!(
                    "cursor   resuming at step {} ({} of {steps})",
                    s.action.name,
                    c.idx + 1
                ),
                None => "cursor   resuming at landing".to_string(),
            });
        }
    }
    Ok(out)
}

/// The line `forge show` prints for a queued task whose stored cursor is
/// usable against its resolution: where its next worker resumes.
pub(crate) fn resume_line(
    stored: Option<&str>,
    actions_json: &str,
    steps: &[&str],
) -> Option<String> {
    match start_from(stored, &workflow_hash(actions_json), steps.len()) {
        Start::At(c) => Some(match steps.get(c.idx) {
            Some(name) => format!("resumes at step {name} ({} of {})", c.idx + 1, steps.len()),
            None => "resumes at landing".to_string(),
        }),
        _ => None,
    }
}

/// Read the stored cursor against the task's resolution of `steps` steps.
pub(crate) fn start_from(stored: Option<&str>, hash: &str, steps: usize) -> Start {
    let Some(raw) = stored.filter(|s| !s.is_empty()) else {
        return Start::Fresh;
    };
    match RunCursor::from_json(raw) {
        None => Start::Restart("the stored run cursor does not parse".into()),
        Some(c) if c.workflow_hash != hash => {
            Start::Restart("the workflow changed under the stored run cursor".into())
        }
        Some(c) if c.idx > steps => {
            Start::Restart("the stored run cursor is past the workflow's last step".into())
        }
        Some(c) => Start::At(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor() -> RunCursor {
        RunCursor {
            workflow_hash: workflow_hash("[1,2,3]"),
            idx: 2,
            attempt: 4,
            owed: BTreeMap::from([(2, "fix it".to_string())]),
            interface: "iface".into(),
            plan: "plan".into(),
        }
    }

    #[test]
    fn a_cursor_round_trips_through_its_json() {
        let c = cursor();
        assert_eq!(RunCursor::from_json(&c.to_json()), Some(c));
    }

    #[test]
    fn a_stored_cursor_for_the_same_resolution_resumes_at_its_index() {
        let c = cursor();
        assert_eq!(
            start_from(Some(&c.to_json()), &c.workflow_hash, 3),
            Start::At(c)
        );
    }

    #[test]
    fn a_cursor_for_another_workflow_hash_restarts_from_zero_with_a_note() {
        let c = cursor();
        match start_from(Some(&c.to_json()), &workflow_hash("[1,2]"), 3) {
            Start::Restart(note) => assert!(note.contains("workflow changed"), "{note}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nothing_stored_is_a_first_run_and_debris_or_a_stale_index_restarts() {
        let c = cursor();
        assert_eq!(start_from(None, &c.workflow_hash, 3), Start::Fresh);
        assert_eq!(start_from(Some(""), &c.workflow_hash, 3), Start::Fresh);
        assert!(matches!(
            start_from(Some("{nope"), &c.workflow_hash, 3),
            Start::Restart(_)
        ));
        assert!(matches!(
            start_from(Some(&c.to_json()), &c.workflow_hash, 1),
            Start::Restart(_)
        ));
    }

    #[test]
    fn show_names_the_step_a_queued_task_resumes_at() {
        let mut c = cursor();
        c.workflow_hash = workflow_hash("[1,2,3]");
        let steps = ["setup", "code", "review"];
        assert_eq!(
            resume_line(Some(&c.to_json()), "[1,2,3]", &steps).as_deref(),
            Some("resumes at step review (3 of 3)")
        );
        assert_eq!(resume_line(Some(&c.to_json()), "[1,2]", &steps), None);
    }

    #[test]
    fn after_a_verified_step_the_cursor_points_at_the_next() {
        let n = cursor().after(5);
        assert_eq!((n.idx, n.attempt), (3, 5));
        assert_eq!(n.plan, "plan");
    }
}
