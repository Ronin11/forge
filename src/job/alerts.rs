//! What a finished job tells the operator without being asked
//! (docs/OPS.md, "What reaches you unasked"): a failed `doctor-daily`, each
//! doctor WARN or FAIL row the first run that sees it, and a step that
//! exited non-zero with nothing on stdout or stderr. Each goes out as an
//! `operator_alert` event, which every channel plugin carries, and onto
//! the message record, which is what keeps a standing row from being sent
//! again by the next run.
use crate::checks::CheckResult;
use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{Alert, JobState};
use anyhow::Result;

/// The marker `checks::run_one_capped` puts in a failed check's tail when
/// the check printed nothing at all.
const SILENT: &str = "[forge] no output captured";

pub(super) struct Finished<'a> {
    pub job_id: i64,
    pub project: &'a str,
    pub workflow: &'a str,
    pub state: JobState,
    pub dry_run: bool,
    pub verdict: &'a [CheckResult],
}

/// Every alert a finished run owes. A dry run owes none; nor does a
/// skipped one, which never ran anything.
pub(super) fn after_job(f: &Forge, j: &Finished<'_>) -> Result<()> {
    if j.dry_run || matches!(j.state, JobState::Dropped | JobState::Skipped) {
        return Ok(());
    }
    for (key, text) in silent_steps(j) {
        announce(f, "tool_silent", &key, j.project, &text);
    }
    if j.workflow == "doctor-daily" {
        doctor_daily(f, j)?;
    }
    Ok(())
}

/// Each failed step or assert that printed nothing: usually its binary or
/// its environment was missing, so the alert names what ran and where.
fn silent_steps(j: &Finished<'_>) -> Vec<(String, String)> {
    j.verdict
        .iter()
        .filter(|r| !r.ok && r.tail.contains(SILENT))
        .map(|r| {
            (
                format!("silent:job:{}:{}", j.job_id, r.name),
                format!(
                    "job {} ({}/{}) {}: the tool produced nothing — {}",
                    j.job_id,
                    j.project,
                    j.workflow,
                    r.name,
                    r.tail.trim().trim_start_matches("[forge] ")
                ),
            )
        })
        .collect()
}

fn doctor_daily(f: &Forge, j: &Finished<'_>) -> Result<()> {
    let now = crate::unix_now();
    let rows = doctor_rows(f, j.job_id)?;
    if j.state == JobState::Ok {
        f.store.clear_alert("doctor-failed")?;
    } else {
        let text = format!(
            "doctor-daily failed (job {}): {}",
            j.job_id,
            failure_text(j.verdict)
        );
        f.store
            .raise_alert(j.project, "doctor_failed", "doctor-failed", &text, now)?;
        // Every failing run is announced, not only the first: a doctor that
        // keeps failing is news each day it does.
        announce(f, "doctor_failed", "doctor-failed", j.project, &text);
    }
    // A run that never reached doctor (no rows and not ok) says nothing
    // about which rows stand, so it clears none.
    if j.state == JobState::Ok || !rows.is_empty() {
        f.store.sync_alerts(j.project, "doctor_row", &rows, now)?;
        for a in f.store.unannounced_alerts(j.project, "doctor_row")? {
            announce_standing(f, &a);
        }
    }
    Ok(())
}

/// The WARN and FAIL rows `doctor-json-to-effects` logged as effects, keyed
/// by row name and status so a row whose numbers move is still one row.
fn doctor_rows(f: &Forge, job_id: i64) -> Result<Vec<(String, String)>> {
    let mut rows = Vec::new();
    for e in f.store.job_effects(job_id)? {
        if e.kind != "row" {
            continue;
        }
        let status = if e.summary.starts_with("WARN:") {
            "warn"
        } else if e.summary.starts_with("FAIL:") {
            "fail"
        } else {
            continue;
        };
        let key = format!("row:{}:{status}", e.target);
        if !rows.iter().any(|(k, _)| *k == key) {
            rows.push((key, format!("doctor {} {}", e.target, e.summary)));
        }
    }
    Ok(rows)
}

/// The failing verdict rows, each by name with its first line.
fn failure_text(verdict: &[CheckResult]) -> String {
    let failed: Vec<String> = verdict
        .iter()
        .filter(|r| !r.ok)
        .map(|r| {
            let first = r.tail.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            format!("{} {}", r.name, first.trim())
        })
        .collect();
    if failed.is_empty() {
        "no step names why".to_string()
    } else {
        failed.join("; ")
    }
}

fn announce_standing(f: &Forge, a: &Alert) {
    f.report.emit(
        0,
        Event::OperatorAlert {
            kind: &a.kind,
            key: &a.key,
            project: &a.project,
            text: &a.text,
            audience: "person",
        },
    );
    if let Err(e) = f.store.record_alert_sent(a) {
        eprintln!("forge: recording alert {}: {e:#}", a.key);
    }
}

/// An alert that is not a standing condition's first sighting: emitted and
/// recorded every time.
fn announce(f: &Forge, kind: &str, key: &str, project: &str, text: &str) {
    announce_standing(
        f,
        &Alert {
            key: key.to_string(),
            project: project.to_string(),
            kind: kind.to_string(),
            text: text.to_string(),
            first_seen: 0,
            last_seen: 0,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, ok: bool, tail: &str) -> CheckResult {
        CheckResult {
            level: "L0".into(),
            name: name.into(),
            ok,
            tail: tail.into(),
            ..Default::default()
        }
    }

    #[test]
    fn only_a_failed_step_with_nothing_on_record_is_silent() {
        let verdict = [
            row("loud", false, "error: boom"),
            row(
                "quiet",
                false,
                "[forge] no output captured; the check exited code 127 with nothing on stdout or stderr; it ran `forge` in /tmp/x",
            ),
            row("fine", true, ""),
        ];
        let j = Finished {
            job_id: 7,
            project: "p",
            workflow: "w",
            state: JobState::Failed,
            dry_run: false,
            verdict: &verdict,
        };
        let silent = silent_steps(&j);
        assert_eq!(silent.len(), 1);
        assert_eq!(silent[0].0, "silent:job:7:quiet");
        assert!(silent[0].1.contains("ran `forge` in /tmp/x"), "{silent:?}");
        assert_eq!(
            failure_text(&verdict),
            "loud error: boom; quiet [forge] no output captured; the check exited code 127 with nothing on stdout or stderr; it ran `forge` in /tmp/x"
        );
    }
}
