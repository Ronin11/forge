//! The schedule trigger: which cron slots are due (`due_schedules`), the
//! tick that starts a job for each (`schedule_tick`), and what the tick
//! logs about a slot a `per_day` cap refuses (`RefusalLog`).

use super::TickRun;
use crate::ctx::Forge;
use crate::job;
use crate::unix_now;
use crate::workflows;
use anyhow::Result;
use croner::Cron;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

/// One project's run workflow with a schedule trigger, resolved for this
/// tick: its cron and the unix second (`Job::trigger_ref`) its last
/// scheduled job recorded, or `None` when it has never started one.
struct Schedule {
    project: String,
    workflow: String,
    cron: Cron,
    last_ref: Option<i64>,
}

/// One schedule due this tick, at the slot it is due for.
struct Due {
    project: String,
    workflow: String,
    slot: i64,
}

/// Pure (docs/JOBS.md, "Triggers"): which of `schedules` are due at `now`,
/// and at which slot. A schedule is due when the latest cron occurrence at
/// or before `now` is newer than its `last_ref` — `None` counts as
/// "before everything", so a schedule that has never run is due at the
/// current slot the first time a tick sees it, never backfilled from
/// whenever the cron would first have matched. A gap of several missed
/// slots (the worker was down) still yields one `Due`: the latest
/// occurrence, never one per missed slot, so a restart cannot flood the
/// queue or re-fire an old one. Ticking again inside the same slot (the
/// poll interval is shorter than the cron's own granularity) yields
/// nothing once `last_ref` catches up to it.
fn due_schedules(now: i64, schedules: Vec<Schedule>) -> Vec<Due> {
    let Some(now_dt) = chrono::DateTime::from_timestamp(now, 0) else {
        return Vec::new();
    };
    schedules
        .into_iter()
        .filter_map(|s| {
            let slot = s
                .cron
                .find_previous_occurrence(&now_dt, true)
                .ok()?
                .timestamp();
            s.last_ref.is_none_or(|r| slot > r).then_some(Due {
                project: s.project,
                workflow: s.workflow,
                slot,
            })
        })
        .collect()
}

/// What the schedule tick has already said about the schedules a `per_day`
/// cap refuses: schedule name (`project/workflow`) -> the second the
/// refusal began. A refusal is the same fact on every tick until the
/// window rolls, so the tick logs it once when it begins and once when it
/// clears and stays silent in between; the state in between is queried
/// from `store::schedule_refusals` (`forge job list`), which the tick keeps
/// up to date.
#[derive(Debug, Default)]
pub(super) struct RefusalLog {
    since: HashMap<String, i64>,
}

impl RefusalLog {
    /// A refusal the tick found at `now`. The line to log if it just
    /// began, `None` if it was already logged.
    fn refused(
        &mut self,
        name: &str,
        now: i64,
        next_allowed: Option<i64>,
        why: &str,
    ) -> Option<String> {
        if self.since.contains_key(name) {
            return None;
        }
        self.since.insert(name.to_string(), now);
        let next = match next_allowed {
            Some(t) => format!("; next start allowed at unix {t}"),
            None => String::new(),
        };
        Some(format!(
            "schedule tick: {name}: {why}{next}; not logged again until it clears"
        ))
    }

    /// The schedule started (or is no longer refused) at `now`. The line to
    /// log if it had been refused, `None` if it had not.
    fn cleared(&mut self, name: &str, now: i64) -> Option<String> {
        let since = self.since.remove(name)?;
        Some(format!(
            "schedule tick: {name}: per_day refusal cleared at unix {now}, after {}s (refused since unix {since})",
            now - since
        ))
    }

    /// When the refusal of `name` began, if it is refused.
    pub(super) fn since(&self, name: &str) -> Option<i64> {
        self.since.get(name).copied()
    }

    /// Take up a refusal an earlier worker recorded, so a restart does not
    /// log it a second time.
    fn adopt(&mut self, name: &str, since: i64) {
        self.since.entry(name.to_string()).or_insert(since);
    }

    /// The names refused so far that are not in `still`.
    fn not_in(&self, still: &HashSet<String>) -> Vec<String> {
        self.since
            .keys()
            .filter(|n| !still.contains(*n))
            .cloned()
            .collect()
    }
}

/// A due slot the `per_day` cap refused: log it if it just began, and
/// record it where the listing reads it.
fn note_refusal(
    f: &Forge,
    log: &mut RefusalLog,
    due: &Due,
    r: &crate::store::PerDayRefused,
    now: i64,
    why: String,
) {
    let name = format!("{}/{}", due.project, due.workflow);
    if let Some(line) = log.refused(&name, now, r.next_allowed, &why) {
        eprintln!("{line}");
    }
    if let Err(e) = f
        .store
        .set_schedule_refusal(&crate::store::ScheduleRefusal {
            project: due.project.clone(),
            workflow: due.workflow.clone(),
            since: log.since(&name).unwrap_or(now),
            next_allowed: r.next_allowed,
            reason: why,
        })
    {
        eprintln!("schedule tick: {name}: {e:#}");
    }
}

/// A refusal whose schedule is no longer due has cleared without a start
/// of ours (`names` are the schedules that still resolve); one whose
/// schedule is gone is dropped without a word.
fn sweep_refusals(
    f: &Forge,
    log: &mut RefusalLog,
    refused: &HashSet<String>,
    names: &HashSet<String>,
    now: i64,
) {
    for name in log.not_in(refused) {
        let Some((project, workflow)) = name.split_once('/') else {
            continue;
        };
        if names.contains(&name)
            && let Some(line) = log.cleared(&name, now)
        {
            eprintln!("{line}");
        }
        log.since.remove(&name);
        if let Err(e) = f.store.clear_schedule_refusal(project, workflow) {
            eprintln!("schedule tick: {name}: {e:#}");
        }
    }
}

/// The worker's schedule trigger (docs/JOBS.md, "Triggers" and "Build
/// order" step 3): for every project, every run workflow with `[trigger]
/// on = "schedule"` that resolves for it, queue one job per cron slot due
/// since its last scheduled job. Called once per pass of the poll loop
/// (`work`, below); cheap when no project has a schedule due, since
/// `due_schedules` alone decides what starts.
pub(super) async fn schedule_tick(f: &Forge, runs: &[TickRun], log: &mut RefusalLog) -> Result<()> {
    let now = unix_now();
    let mut schedules = Vec::new();
    let mut resolved: HashMap<
        (String, String),
        (&workflows::Workflow, workflows::JobSource, &str),
    > = HashMap::new();
    for run in runs {
        let Some(trigger) = run.wf.trigger.as_ref() else {
            continue;
        };
        if trigger.on != workflows::TriggerOn::Schedule {
            continue;
        }
        // `workflows::parse` already refused an unparseable cron at load
        // time (with the file and the line), so this always parses; a
        // defensive skip rather than a panic if it somehow did not.
        let Some(cron) = trigger.cron.as_deref().and_then(|e| Cron::from_str(e).ok()) else {
            continue;
        };
        let last_ref = match f.store.last_scheduled_job(&run.project, &run.name) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("schedule tick: {}/{}: {e:#}", run.project, run.name);
                continue;
            }
        };
        schedules.push(Schedule {
            project: run.project.clone(),
            workflow: run.name.clone(),
            cron,
            last_ref,
        });
        resolved.insert(
            (run.project.clone(), run.name.clone()),
            (&run.wf, run.source, run.landed_sha.as_str()),
        );
    }
    match f.store.schedule_refusals() {
        Ok(rows) => {
            for r in rows {
                log.adopt(&format!("{}/{}", r.project, r.workflow), r.since);
            }
        }
        Err(e) => eprintln!("schedule tick: {e:#}"),
    }
    let names: HashSet<String> = resolved
        .keys()
        .map(|(project, workflow)| format!("{project}/{workflow}"))
        .collect();
    let mut refused: HashSet<String> = HashSet::new();
    for due in due_schedules(now, schedules) {
        let Some((wf, source, landed_sha)) =
            resolved.get(&(due.project.clone(), due.workflow.clone()))
        else {
            continue;
        };
        let name = format!("{}/{}", due.project, due.workflow);
        match job::start_scheduled(
            f,
            &due.project,
            &due.workflow,
            landed_sha,
            wf,
            *source,
            due.slot,
        )
        .await
        {
            Ok(id) => {
                if let Some(line) = log.cleared(&name, now) {
                    eprintln!("{line}");
                }
                if let Err(e) = f.store.clear_schedule_refusal(&due.project, &due.workflow) {
                    eprintln!("schedule tick: {name}: {e:#}");
                }
                eprintln!(
                    "======== job {id} starting (schedule {} on {})",
                    due.workflow, due.project
                );
            }
            Err(e) => match e.downcast_ref::<crate::store::PerDayRefused>() {
                Some(r) => {
                    refused.insert(name.clone());
                    note_refusal(f, log, &due, r, now, format!("{e:#}"));
                }
                None => eprintln!("schedule tick: {}/{}: {e:#}", due.project, due.workflow),
            },
        }
    }
    sweep_refusals(f, log, &refused, &names, now);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minute-aligned unix second: `Cron::find_previous_occurrence`
    /// works in whole seconds, so every fixture below starts from one to
    /// keep "the minute" unambiguous.
    fn minute_boundary() -> i64 {
        let t = unix_now();
        t - (t % 60)
    }

    fn every_minute() -> Cron {
        Cron::from_str("* * * * *").unwrap()
    }

    fn schedule(last_ref: Option<i64>) -> Schedule {
        Schedule {
            project: "equitizr".into(),
            workflow: "snapshot".into(),
            cron: every_minute(),
            last_ref,
        }
    }

    #[test]
    fn due_once_at_the_minute() {
        let now = minute_boundary();
        let due = due_schedules(now, vec![schedule(None)]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].project, "equitizr");
        assert_eq!(due[0].workflow, "snapshot");
        assert_eq!(due[0].slot, now);
    }

    #[test]
    fn not_due_twice_in_the_same_minute() {
        let now = minute_boundary();
        // The tick already recorded this minute's slot; ticking again a
        // few seconds later, still inside the same minute, finds nothing.
        let due = due_schedules(now + 30, vec![schedule(Some(now))]);
        assert!(
            due.is_empty(),
            "{:?}",
            due.iter().map(|d| d.slot).collect::<Vec<_>>()
        );
    }

    #[test]
    fn catch_up_runs_the_latest_missed_slot_only() {
        let now = minute_boundary();
        // The worker was down for five minutes; only one job starts, for
        // the latest slot, not one per minute missed.
        let due = due_schedules(now, vec![schedule(Some(now - 5 * 60))]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].slot, now);
    }

    #[test]
    fn a_schedule_whose_slot_has_not_yet_come_is_not_due() {
        let now = minute_boundary();
        // Its last job already covers the latest slot at or before `now`.
        let due = due_schedules(now, vec![schedule(Some(now))]);
        assert!(due.is_empty());
    }

    #[test]
    fn a_cron_is_evaluated_in_utc() {
        // 2026-09-21T08:30:00Z: "0 7 * * *" last fired at 07:00 UTC that day,
        // whatever zone the machine is in.
        let now = 1_789_979_400;
        let s = Schedule {
            project: "p".into(),
            workflow: "w".into(),
            cron: Cron::from_str("0 7 * * *").unwrap(),
            last_ref: None,
        };
        let due = due_schedules(now, vec![s]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].slot, 1_789_974_000);
    }

    #[test]
    fn every_five_minutes_only_matches_its_own_slots() {
        let now = minute_boundary() - (minute_boundary() % 300) + 300; // a "*/5" boundary
        let cron = Cron::from_str("*/5 * * * *").unwrap();
        let s = Schedule {
            project: "p".into(),
            workflow: "w".into(),
            cron,
            last_ref: None,
        };
        let due = due_schedules(now, vec![s]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].slot, now);
        assert_eq!(due[0].slot % 300, 0);
    }

    #[test]
    fn a_refusal_is_logged_when_it_begins_and_when_it_clears_and_not_between() {
        let mut log = RefusalLog::default();
        let name = "forge/gc-nightly";
        let why =
            "gc-nightly has started 1 time(s) in the last 24 hours and its per_day limit is 1";
        let begun = log
            .refused(name, 100, Some(86_500), why)
            .expect("logged once");
        assert!(begun.contains(name) && begun.contains(why), "{begun}");
        assert!(
            begun.contains("86500"),
            "names the next allowed time: {begun}"
        );
        for tick in 1..=20 {
            assert_eq!(log.refused(name, 100 + 30 * tick, Some(86_500), why), None);
        }
        assert_eq!(log.since(name), Some(100), "the refusal still began at 100");
        let cleared = log.cleared(name, 86_500).expect("logged once");
        assert!(
            cleared.contains(name) && cleared.contains("cleared"),
            "{cleared}"
        );
        assert_eq!(log.cleared(name, 86_530), None);
        // A refusal after that is a new one and is logged again.
        assert!(log.refused(name, 90_000, None, why).is_some());
    }

    #[test]
    fn schedules_are_deduplicated_independently_and_a_restart_adopts_the_recorded_refusal() {
        let mut log = RefusalLog::default();
        assert!(log.refused("a/x", 10, None, "why").is_some());
        assert!(log.refused("a/y", 11, None, "why").is_some());
        assert_eq!(log.refused("a/x", 12, None, "why"), None);
        let mut restarted = RefusalLog::default();
        restarted.adopt("a/x", 10);
        assert_eq!(restarted.refused("a/x", 40, None, "why"), None);
        assert_eq!(restarted.since("a/x"), Some(10));
        let still: HashSet<String> = ["a/x".to_string()].into();
        assert_eq!(log.not_in(&still), vec!["a/y".to_string()]);
    }
}
