//! Watch 8, `jobs_stale`: a job left `needs_human` or `failed` for more
//! than 48 hours, with nothing since that closed it (an answer, or a
//! later `ok` run of the same workflow), named with its failing check.
//! A check whose process printed nothing is named as such, `tool produced
//! nothing`, not as a fault of the check (doctor-daily 09-30 and 10-01,
//! drift-weekly 09-28 and 10-05).

use super::{Check, Status, check, clip};
use crate::checks::CheckResult;
use crate::store::Store;

/// How long a failed job may sit before it is a row.
pub(super) const STALE_SECS: i64 = 48 * 3600;

/// The job's first failing check in words, or that none was recorded.
pub(super) fn failing_check(verdict_json: &str) -> String {
    let verdict: Vec<CheckResult> = serde_json::from_str(verdict_json).unwrap_or_default();
    verdict
        .iter()
        .find(|c| !c.ok)
        .map_or("no check recorded which one failed".to_string(), |c| {
            clip(&c.failure_text(), 200)
        })
}

pub(super) fn watch(store: &Store, now: i64) -> Check {
    let jobs = match store.stale_jobs(now - STALE_SECS) {
        Ok(j) => j,
        Err(e) => return check("jobs_stale", Status::Warn, format!("{e:#}"), ""),
    };
    if jobs.is_empty() {
        return check(
            "jobs_stale",
            Status::Ok,
            "no job left needs_human or failed for more than 48h",
            "",
        );
    }
    let lines: Vec<String> = jobs
        .iter()
        .map(|j| {
            format!(
                "job {} ({}/{}) {} since {}: {}",
                j.id,
                j.project,
                j.workflow,
                j.state.as_str(),
                super::super::ymd(j.finished_at.unwrap_or(j.started_at)),
                failing_check(&j.verdict_json)
            )
        })
        .collect();
    check(
        "jobs_stale",
        Status::Warn,
        lines.join("; "),
        format!(
            "forge job show {} names what failed; answer its question or fix and rerun the workflow",
            jobs[0].id
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, store};
    use super::*;

    fn job(s: &Store, workflow: &str, state: &str, at: i64, verdict: &str) {
        let c = s.lock();
        c.execute(
            "INSERT OR IGNORE INTO projects (name, purpose, created_at) VALUES ('forge', 'p', 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO jobs (project, workflow, trigger_kind, trigger_ref, state, started_at,
                               finished_at, verdict_json)
             VALUES ('forge', ?1, 'schedule', ?3, ?2, ?3, ?3, ?4)",
            rusqlite::params![workflow, state, at, verdict],
        )
        .unwrap();
    }

    #[test]
    fn a_job_needing_a_human_for_three_days_warns_and_names_an_empty_check() {
        let (_d, s) = store();
        let v = r#"[{"level":"L0","name":"clean","ok":false,"exit":1,"ms":0,"timed_out":false,
            "tail":"[forge] no output captured; the check exited code 1 with nothing on stdout or stderr"}]"#;
        job(&s, "doctor-daily", "needs_human", NOW - 3 * 86_400, v);
        let c = watch(&s, NOW);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(
            c.detail.contains("(forge/doctor-daily) needs_human"),
            "{}",
            c.detail
        );
        assert!(
            c.detail
                .contains("tool produced nothing (check clean, exit 1)"),
            "{}",
            c.detail
        );
    }

    #[test]
    fn a_recent_failure_or_one_a_later_run_fixed_is_ok() {
        let (_d, s) = store();
        job(&s, "drift-weekly", "failed", NOW - 3600, "[]");
        job(&s, "doctor-daily", "failed", NOW - 5 * 86_400, "[]");
        job(&s, "doctor-daily", "ok", NOW - 4 * 86_400, "[]");
        assert_eq!(watch(&s, NOW).status, Status::Ok);
    }
}
