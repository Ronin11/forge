use crate::support::*;
use std::path::Path;
use std::process::Output;

fn supervised(e: &Env, supervisor: &str, task: &str) -> Output {
    let mut c = e.with_role("needsinput.sh", "SUPERVISOR", supervisor);
    c.env("FORGE_SUPERVISOR", "1");
    let o = c
        .args(["run", e.repo.to_str().unwrap(), task, "--retries", "0"])
        .output()
        .unwrap();
    eprintln!(
        "--- supervised by {supervisor} ---\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

#[test]
fn the_supervisor_answers_a_question_with_citations_and_the_answer_lands() {
    let e = Env::new();
    let o = supervised(&e, "supervisor-answer.sh", "write 42 to the answer file");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("supervisor reading the record (opus, 30 turns)"),
        "{err}"
    );
    assert!(
        err.contains("✓ L0 untouched") && err.contains("✓ L0 cites-real-things"),
        "{err}"
    );
    assert!(
        err.contains(
            "supervisor answered (citing hello.sh, forge.toml) and re-queued the task as 2"
        ),
        "{err}"
    );
    assert_eq!(e.task(1).0, "blocked");
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "the ruling is an attempt on the record: {a:?}");
    assert_eq!(a[1].1, "succeeded");
    assert!(a[1].2.starts_with("supervisor: answer"), "{}", a[1].2);
    let doc: serde_json::Value = e.trace_json("2");
    assert_eq!(doc["task"]["retry_of"], 1);
    let text = doc["task"]["text"].as_str().unwrap();
    assert!(
        text.contains("Supervisor's answer to a question from an earlier attempt (citing hello.sh, forge.toml): Use answer.txt"),
        "{text}"
    );
    // The retried task runs and lands; the decision shows the outcome.
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    assert_eq!(e.task(2).0, "succeeded");
    let ds: serde_json::Value = e.decisions_json();
    let d = &ds.as_array().unwrap()[0];
    assert_eq!(d["answered_by"], "supervisor");
    assert_eq!(d["citations"], "hello.sh, forge.toml");
    assert_eq!(d["retry_id"], 2);
    assert_eq!(d["outcome"], "succeeded");
    let text = String::from_utf8_lossy(&e.forge("ok.sh", &["decisions"]).stdout).to_string();
    assert!(
        text.contains("A (supervisor, citing hello.sh, forge.toml): Use answer.txt"),
        "{text}"
    );
    assert!(text.contains("→ task 2 succeeded"), "{text}");
    // Nothing is left for the human.
    let reqs: serde_json::Value = e.requests_json();
    assert!(reqs.as_array().unwrap().is_empty(), "{reqs}");
}

#[test]
fn the_supervisor_escalates_what_the_record_does_not_settle_and_refuses_bad_citations() {
    let e = Env::new();
    let o = supervised(&e, "supervisor-escalate.sh", "write 42 to the answer file");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("supervisor escalated: the file name is a naming preference"),
        "{err}"
    );
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(
        reason.contains("[supervisor escalated: the file name is a naming preference"),
        "{reason}"
    );
    // The question is still the human's: it shows in requests, and no task was queued.
    let reqs: serde_json::Value = e.requests_json();
    assert_eq!(reqs.as_array().unwrap().len(), 1, "{reqs}");
    assert_eq!(reqs[0]["kind"], "question");
    let ids: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["log", "--json"]).stdout).unwrap();
    assert_eq!(ids.as_array().unwrap().len(), 1);

    // A citation that resolves to nothing is refused; the question goes to the human.
    let o = supervised(
        &e,
        "supervisor-lost.sh",
        "write 42 to the answer file again",
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("✗ L0 cites-real-things"), "{err}");
    assert!(
        err.contains("supervisor escalated: its ruling failed cites-real-things: citations that resolve to nothing: docs/CONVENTIONS.md"),
        "{err}"
    );
    let (state, reason, _) = e.task(2);
    assert_eq!(state, "blocked");
    assert!(reason.contains("supervisor escalated"), "{reason}");
    let ds: serde_json::Value = e.decisions_json();
    assert!(
        ds.as_array().unwrap().is_empty(),
        "a refused ruling is no decision: {ds}"
    );
}

#[test]
fn the_supervisor_marks_a_task_superseded_by_one_that_already_landed() {
    let e = Env::new();
    assert!(
        e.run("ok.sh", &[]).status.success(),
        "task 1 lands the work"
    );
    let o = supervised(
        &e,
        "supervisor-superseded.sh",
        "write 42 to the answer file",
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("✓ L0 supersedes-with-a-landed-task"), "{err}");
    assert!(
        err.contains("supervisor marked the task superseded by task 1"),
        "{err}"
    );
    let (state, reason, _) = e.task(2);
    assert_eq!(state, "failed");
    assert!(
        reason.starts_with("superseded by task 1 (supervisor)"),
        "{reason}"
    );
    let reqs: serde_json::Value = e.requests_json();
    assert!(
        reqs.as_array().unwrap().is_empty(),
        "nothing is left for the human: {reqs}"
    );
    let ds: serde_json::Value = e.decisions_json();
    assert_eq!(
        ds[0]["answer"]
            .as_str()
            .map(|a| a.starts_with("superseded by task 1")),
        Some(true)
    );
    // Citing a task that did not succeed, or is not this repository's, is refused.
    let o = supervised(
        &e,
        "supervisor-superseded.sh",
        "write 42 to the answer file again",
    );
    // task 1 still succeeded, so this one is superseded too; the refusal case
    // needs a citation to a failed task: task 2 failed above.
    assert!(String::from_utf8_lossy(&o.stderr).contains("superseded by task 1"));
}

#[test]
fn a_supervisor_that_crashes_is_an_agent_failure_and_the_question_escalates() {
    let e = Env::new();
    let o = supervised(&e, "supervisor-crash.sh", "write 42 to the answer file");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("supervisor escalated: its run failed: agent exit 1"),
        "{err}"
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(a[1].1, "agent_failed", "{a:?}");
    assert_eq!(a[1].2, "agent exit 1");
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(
        reason.contains("[supervisor escalated: its run failed: agent exit 1]"),
        "{reason}"
    );
    let ds: serde_json::Value = e.decisions_json();
    assert!(ds.as_array().unwrap().is_empty(), "{ds}");
}

#[test]
fn the_supervisor_files_a_prerequisite_and_requeues_the_task_behind_it() {
    let e = Env::new();
    let o = supervised(&e, "supervisor-prereq.sh", "write 42 to the answer file");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("supervisor filed prerequisite task 2 and re-queued the task as 3 behind it"),
        "{err}"
    );
    let doc: serde_json::Value = e.trace_json("2");
    assert!(
        doc["task"]["text"]
            .as_str()
            .unwrap()
            .starts_with("Make hello.sh print exactly the word hello")
    );
    assert_eq!(doc["task"]["workflow"], "direct");
    assert!(doc["task"]["retry_of"].is_null());
    let doc: serde_json::Value = e.trace_json("3");
    assert_eq!(doc["task"]["retry_of"], 1);
    assert_eq!(doc["task"]["after"], serde_json::json!([2]));
    assert!(
        doc["task"]["text"]
            .as_str()
            .unwrap()
            .contains("re-queued behind prerequisite task 2, which makes hello.sh print"),
        "{}",
        doc["task"]["text"]
    );
    let ds: serde_json::Value = e.decisions_json();
    assert_eq!(
        ds[0]["answer"]
            .as_str()
            .map(|a| a.starts_with("prerequisite task 2:")),
        Some(true)
    );
    assert_eq!(ds[0]["retry_id"], 3);
}

#[test]
fn the_supervisor_stops_answering_after_its_share_of_a_piece_of_work() {
    // per_lineage is 2 by default: the third question in a lineage is the human's.
    let e = Env::new();
    let o = supervised(&e, "supervisor-answer.sh", "write 42 to the answer file");
    assert!(String::from_utf8_lossy(&o.stderr).contains("re-queued the task as 2"));
    // One worker pass drains the queue: task 2 asks again (the coder fake
    // never changes) and the supervisor answers again as 3; task 3 asks
    // again and the supervisor must step aside.
    let mut c = e.with_role("needsinput.sh", "SUPERVISOR", "supervisor-answer.sh");
    c.env("FORGE_SUPERVISOR", "1");
    let o = c.args(["work", "--once"]).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    eprintln!("--- work ---\n{err}");
    assert!(err.contains("re-queued the task as 3"), "{err}");
    assert!(
        err.contains("supervisor escalated: the supervisor has already answered 2 time(s) in this piece of work"),
        "{err}"
    );
    let (state, reason, _) = e.task(3);
    assert_eq!(state, "blocked");
    assert!(reason.contains("supervisor escalated"), "{reason}");
}

#[test]
fn forge_supervise_by_hand_answers_a_blocked_task_and_re_queues_it() {
    let e = Env::new();
    // Blocked with the supervisor off: nothing answers it automatically.
    let o = e.forge(
        "needsinput.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to the answer file",
            "--retries",
            "0",
        ],
    );
    assert!(!o.status.success(), "a blocked task exits non-zero");
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(!reason.contains("supervisor"), "{reason}");
    assert_eq!(e.attempts(1).len(), 1, "no supervisor attempt ran yet");

    // A human runs the supervisor on it by hand.
    let mut c = e.with_role("ok.sh", "SUPERVISOR", "supervisor-answer.sh");
    c.env("FORGE_SUPERVISOR", "1");
    let o = c.args(["supervise", "1"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains(
            "supervisor answered (citing hello.sh, forge.toml) and re-queued the task as 2"
        ),
        "{err}"
    );
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("answered; re-queued as task 2"),
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );

    assert_eq!(e.task(1).0, "blocked", "the blocked task itself stands");
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "the ruling is an attempt on the record: {a:?}");
    assert_eq!(a[1].1, "succeeded");
    assert!(a[1].2.starts_with("supervisor: answer"), "{}", a[1].2);
    let doc: serde_json::Value = e.trace_json("2");
    assert_eq!(doc["task"]["retry_of"], 1);
    let ds: serde_json::Value = e.decisions_json();
    assert_eq!(ds[0]["answered_by"], "supervisor");
    assert_eq!(ds[0]["retry_id"], 2);

    // The re-queued task runs and lands the answer.
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    assert_eq!(e.task(2).0, "succeeded");
}

#[test]
fn the_supervisor_accepts_a_demotion_that_names_no_defect_and_the_branch_lands() {
    let e = Env::new();
    let mut c = e.cmd("ok.sh");
    c.env("FORGE_SUPERVISOR", "1");
    for (role, fake) in [
        ("REVIEW", "reviewer-approves-wrongly.sh"),
        ("SUPERVISOR", "supervisor-accept.sh"),
    ] {
        c.env(
            format!("FORGE_CLAUDE_BIN_{role}"),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(fake),
        );
    }
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "reviewed",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("review demoted: No defect found"), "{err}");
    assert!(
        err.contains(
            "supervisor accepted the branch (citing answer.txt, forge.toml) and landed it"
        ),
        "{err}"
    );
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    let ds: serde_json::Value = e.decisions_json();
    assert_eq!(ds[0]["answered_by"], "supervisor");
    assert!(
        ds[0]["answer"]
            .as_str()
            .unwrap()
            .starts_with("accepted the branch despite the demotion")
    );
    assert_eq!(ds[0]["outcome"], "succeeded");
    // Nothing waits for the human, and nothing was rebuilt.
    assert!(e.requests_json().as_array().unwrap().is_empty());
    let log: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["log", "--json"]).stdout).unwrap();
    assert_eq!(log.as_array().unwrap().len(), 1);
}

#[test]
fn the_supervisor_accepts_a_question_whose_checks_already_passed_and_lands_it() {
    // The coder commits a real answer, and the repository's checks pass
    // on it, but it asks a question instead of returning cleanly. That
    // is not a defect the record found; the supervisor may accept it,
    // the same as it would a review demotion that names no defect.
    let e = Env::new();
    let mut c = e.cmd("commitneedsinput.sh");
    c.env("FORGE_SUPERVISOR", "1");
    c.env(
        "FORGE_CLAUDE_BIN_SUPERVISOR",
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fakes")
            .join("supervisor-accept-question.sh"),
    );
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("needs input: Should ANSWER.txt"), "{err}");
    assert!(
        err.contains(
            "supervisor accepted the branch (citing answer.txt, forge.toml) and landed it"
        ),
        "{err}"
    );
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    assert_eq!(
        origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    let ds: serde_json::Value = e.decisions_json();
    assert_eq!(ds[0]["answered_by"], "supervisor");
    assert!(
        ds[0]["answer"]
            .as_str()
            .unwrap()
            .starts_with("accepted the branch; the checks passed and the question is settled"),
        "{}",
        ds[0]["answer"]
    );
    assert_eq!(ds[0]["outcome"], "succeeded");
    // Nothing waits for the human, and nothing was rebuilt.
    assert!(e.requests_json().as_array().unwrap().is_empty());
}

/// A question addressed to a named contact (an intake interview's
/// person, say) is not the operator's, so it is not the supervisor's
/// either: it must be skipped without an attempt, leaving it for the
/// channel plugin to deliver and answer.
#[test]
fn a_question_addressed_to_someone_else_never_gets_a_supervisor_attempt() {
    let e = Env::new();
    // A supervisor fake that would answer is wired in, so a failure to
    // skip would show up as an answer, not silence.
    let mut c = e.with_role("needsinput-to.sh", "SUPERVISOR", "supervisor-answer.sh");
    c.env("FORGE_SUPERVISOR", "1");
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to the answer file",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("question addressed to alice; not the supervisor's to answer"),
        "{err}"
    );
    assert!(!err.contains("supervisor reading the record"), "{err}");
    assert!(!err.contains("supervisor answered"), "{err}");
    let (state, _reason, _) = e.task(1);
    assert_eq!(state, "blocked");
    let attempts = e.attempts(1);
    assert_eq!(
        attempts.len(),
        1,
        "no supervisor attempt should have run: {attempts:?}"
    );
    // The question is still for the channel plugin to pick up.
    let reqs: serde_json::Value = e.requests_json();
    assert_eq!(reqs.as_array().unwrap().len(), 1, "{reqs}");
    assert_eq!(reqs[0]["to"], "alice");
    let ds: serde_json::Value = e.decisions_json();
    assert!(ds.as_array().unwrap().is_empty(), "{ds}");
}
