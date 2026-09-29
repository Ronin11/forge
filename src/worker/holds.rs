//! Initiatives the worker holds: which ones (budget spent, or a stop
//! rule's streak reached), and the one announcement each hold gets.

use crate::ctx::Forge;
use crate::store::TaskState;
use anyhow::Result;

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

/// One line for each initiative that just entered `held` with a reason not
/// already recorded for it, naming why, how many of its tasks are stuck
/// queued behind it and what lifts it: "initiative 56 held: stop rule:
/// L1 test: a::b (streak 3); 24 task(s) queued behind it; forge
/// initiative set 56 --stop-after <n> to continue, or fix the rule". Each
/// is also recorded as an `initiative_held` event for a person (the
/// notify and signal plugins carry it), since only a person decides to
/// continue. What was announced lives in the store (`initiative_holds`),
/// not in this process's memory: a successor started by a self-deploy
/// reads the same record, so it never repeats an announcement a
/// predecessor already made (see `work`, which prints whatever this
/// returns). A hold's record is dropped as soon as it leaves `held`, so a
/// later, separate hold on the same initiative — even with the same
/// reason — is announced again.
pub(super) fn new_holds(f: &Forge, held: &[i64]) -> Vec<String> {
    let mut lines = Vec::new();
    if let Ok(previously) = f.store.announced_holds() {
        for id in previously {
            if !held.contains(&id) {
                let _ = f.store.clear_announced_hold(id);
            }
        }
    }
    for &id in held {
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
        let reason = crate::view::initiative_hold_reason(f, &ini)
            .ok()
            .flatten()
            .unwrap_or_else(|| "held".to_string());
        if f.store.announced_hold_reason(id).ok().flatten().as_deref() == Some(reason.as_str()) {
            continue;
        }
        if f.store
            .record_hold_announced(id, &reason, crate::unix_now())
            .is_err()
        {
            continue;
        }
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

        let held = vec![ini_id];
        let first = new_holds(&f, &held);
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
            assert!(new_holds(&f, &held).is_empty());
        }

        // The hold lifts (no longer in `held`), then recurs: announced again.
        assert!(new_holds(&f, &[]).is_empty());
        let again = new_holds(&f, &held);
        assert_eq!(again.len(), 1);
        assert_eq!(held_events().len(), 2);
    }

    /// Two worker instances (a self-deploy's predecessor and successor)
    /// over one store: the second never repeats what the first already
    /// announced, because what was announced lives in the store, not in
    /// either process's own memory (`announced` used to be a `HashSet`
    /// that started empty in every successor).
    #[test]
    fn a_successor_worker_reads_the_predecessors_announced_hold() {
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
                budget_usd: Some(50.0),
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

        // The predecessor worker sees the hold and announces it.
        let held = vec![ini_id];
        let predecessor = new_holds(&f, &held);
        assert_eq!(predecessor.len(), 1);

        // A second `Forge` over the same store (the self-deploy's
        // successor, its own `new_holds` call starting with no in-memory
        // history of its own): same hold, nothing new to say.
        let successor = crate::ctx::Forge::open_with(
            f.paths.clone(),
            crate::store::Store::open(&f.paths.home.join("forge.db")).unwrap(),
        )
        .unwrap();
        assert!(
            new_holds(&successor, &held).is_empty(),
            "a successor must not re-announce a hold its predecessor already announced"
        );

        // The event log has exactly the predecessor's one announcement.
        let held_events = || -> Vec<serde_json::Value> {
            std::fs::read_to_string(f.paths.home.join("events.jsonl"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v["type"] == "initiative_held")
                .collect()
        };
        assert_eq!(held_events().len(), 1);
    }
}
