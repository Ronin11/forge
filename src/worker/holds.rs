//! Initiatives the worker holds: which ones (budget spent, or a stop
//! rule's streak reached), and the one announcement each hold gets.

use crate::ctx::Forge;
use crate::store::TaskState;
use anyhow::Result;
use std::collections::HashSet;

/// Every initiative currently holding new claims: its budget is spent, or
/// its trailing run of same-rule failures reached its stop rule (see
/// docs/PROJECTS.md, "Stop rule and budget"). Only initiatives with a
/// queued task are worth checking.
pub(crate) fn held_initiatives(f: &Forge) -> Result<Vec<i64>> {
    let mut held = Vec::new();
    for id in f.store.initiatives_with_queued_tasks()? {
        if let Some(ini) = f.store.initiative(id)?
            && crate::view::initiative_hold(f, &ini)?.is_some()
        {
            held.push(id);
        }
    }
    Ok(held)
}

/// One line for each initiative that just entered `held` (an id not seen
/// in `announced` before), naming why, how many of its tasks are stuck
/// queued behind it and what lifts it: "initiative 56 held: stop rule:
/// L1 test: a::b (streak 3); 24 task(s) queued behind it; forge
/// initiative set 56 --stop-after <n> to continue, or fix the rule". Each
/// is also recorded as an `initiative_held` event for a person (the
/// notify and signal plugins carry it), since only a person decides to
/// continue. Nothing for a hold already announced, so a slow poll
/// interval does not turn into a flood (see `work`, which prints whatever
/// this returns). `announced` drops an id as soon as it leaves `held`, so
/// a later, separate hold on the same initiative is announced again.
pub(super) fn new_holds(f: &Forge, held: &[i64], announced: &mut HashSet<i64>) -> Vec<String> {
    let mut lines = Vec::new();
    for &id in held {
        if announced.contains(&id) {
            continue;
        }
        let Ok(Some(ini)) = f.store.initiative(id) else {
            continue;
        };
        let Ok(tasks) = f.store.initiative_tasks(id) else {
            continue;
        };
        let queued: Vec<&crate::store::Task> = tasks
            .iter()
            .filter(|t| t.state == TaskState::Queued)
            .collect();
        if queued.is_empty() {
            continue;
        }
        announced.insert(id);
        let reason = crate::view::initiative_hold_reason(f, &ini)
            .ok()
            .flatten()
            .unwrap_or_else(|| "held".to_string());
        // The task the hold follows from: the latest to finish, else the
        // first still waiting.
        let task_id = tasks
            .iter()
            .filter(|t| t.finished_at.is_some())
            .max_by_key(|t| (t.finished_at, t.id))
            .or(queued.first().copied())
            .map_or(0, |t| t.id);
        let ev = crate::report::Event::InitiativeHeld {
            id,
            project: &ini.project,
            reason: &reason,
            queued: queued.len(),
            audience: "person",
        };
        lines.push(ev.summary());
        f.report.record(task_id, &ev);
    }
    announced.retain(|id| held.contains(id));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A held initiative with a queued task is announced the first time
    /// `new_holds` sees it, never again while the hold continues (even
    /// across many polls), and again once it leaves `held` and re-enters
    /// (docs/PROJECTS.md, "Stop rule and budget"): the claim loop calls
    /// this every poll, so this is what keeps a slow poll from spamming.
    #[test]
    fn new_holds_announces_a_held_initiative_once_per_hold() {
        let (_dir, f) = super::super::tests::fixture();
        f.store
            .create_project(&crate::store::Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        let ini_id = f
            .store
            .create_initiative(&crate::store::Initiative {
                project: "demo".into(),
                outcome: "o".into(),
                budget_usd: Some(0.0),
                stop_after_same_rule: 3,
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        let mut t = super::super::tests::task_on("direct");
        t.project = Some("demo".into());
        t.initiative = Some(ini_id);
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();

        let mut announced = HashSet::new();
        let held = vec![ini_id];
        let first = new_holds(&f, &held, &mut announced);
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0],
            format!(
                "initiative {ini_id} held: budget: $0.00 of $0.00; 1 task(s) queued behind it; \
                 forge initiative set {ini_id} --budget <usd> to continue"
            )
        );

        // Recorded once, as an event for a person, under the queued task.
        let held_events = || -> Vec<serde_json::Value> {
            std::fs::read_to_string(f.paths.home.join("events.jsonl"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v["type"] == "initiative_held")
                .collect()
        };
        let events = held_events();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["id"], ini_id);
        assert_eq!(events[0]["audience"], "person");
        assert_eq!(events[0]["queued"], 1);
        assert_eq!(events[0]["task"], t.id);
        assert_eq!(events[0]["text"], first[0].as_str());

        // Same hold, three more polls: nothing new to say.
        for _ in 0..3 {
            assert!(new_holds(&f, &held, &mut announced).is_empty());
        }

        // The hold lifts (no longer in `held`), then recurs: announced again.
        assert!(new_holds(&f, &[], &mut announced).is_empty());
        let again = new_holds(&f, &held, &mut announced);
        assert_eq!(again.len(), 1);
        assert_eq!(held_events().len(), 2);
    }
}
