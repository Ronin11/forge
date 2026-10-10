//! Watch 1, `supervisor`: the supervisor ran in the last day, none of its
//! runs succeeded, and a task is blocked waiting on it. From 09-29 to
//! 10-10 every supervisor run failed and every blocked task fell back to
//! the operator unread; the attempts rows had it all along.

use super::{Check, DAY, Status, check, clip};
use crate::store::Store;

pub(super) fn watch(store: &Store, now: i64) -> Check {
    let runs = match store.step_runs_since("supervisor", now - DAY) {
        Ok(r) => r,
        Err(e) => return check("supervisor", Status::Warn, format!("{e:#}"), ""),
    };
    let blocked = store.blocked_count().unwrap_or(0);
    let ok = runs.iter().filter(|r| r.ok).count();
    match runs.first() {
        Some(last) if ok == 0 && blocked > 0 => check(
            "supervisor",
            Status::Warn,
            format!(
                "{} supervisor run(s) in the last 24h, none succeeded, {blocked} task(s) blocked; \
                 the last (task {}): {}",
                runs.len(),
                last.task_id,
                clip(&last.reason, 300)
            ),
            format!(
                "forge trace {} shows the run; a blocked task falls back to the operator until \
                 the supervisor can read it",
                last.task_id
            ),
        ),
        Some(_) => check(
            "supervisor",
            Status::Ok,
            format!(
                "{ok} of {} run(s) in the last 24h succeeded; {blocked} task(s) blocked",
                runs.len()
            ),
            "",
        ),
        None => check(
            "supervisor",
            Status::Ok,
            format!("no runs in the last 24h; {blocked} task(s) blocked"),
            "",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, attempt, store, task};
    use super::*;

    #[test]
    fn every_run_failing_while_a_task_is_blocked_warns_with_the_last_reason() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b1", "blocked");
        attempt(&s, t, "supervisor", "agent_failed", "exit 1: no model", NOW - 7200);
        attempt(&s, t, "supervisor", "agent_failed", "exit 1: refused", NOW - 60);
        let c = watch(&s, NOW);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("2 supervisor run(s)"), "{}", c.detail);
        assert!(c.detail.contains("exit 1: refused"), "{}", c.detail);
    }

    #[test]
    fn one_success_or_no_blocked_task_is_ok() {
        let (_d, s) = store();
        let t = task(&s, "forge", "b1", "blocked");
        attempt(&s, t, "supervisor", "agent_failed", "exit 1", NOW - 7200);
        attempt(&s, t, "supervisor", "succeeded", "", NOW - 60);
        assert_eq!(watch(&s, NOW).status, Status::Ok);

        let (_d, s) = store();
        let t = task(&s, "forge", "b1", "failed");
        attempt(&s, t, "supervisor", "agent_failed", "exit 1", NOW - 60);
        assert_eq!(watch(&s, NOW).status, Status::Ok);
    }
}
