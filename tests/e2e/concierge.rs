//! `forge ask` and the `concierge` directive (docs/INTAKE.md, "The front
//! door is not the interview"): a customer message is a request, a
//! question, a need, or unclear, from the project's purpose, brief,
//! backlog, deploy targets and last tasks; `forge ask` acts on whichever
//! it decided and records it.

use crate::support::*;

/// One row from `tasks`, the columns `Env::task` does not carry.
fn row(e: &Env, id: i64) -> (String, String, Option<String>, Option<String>) {
    e.db()
        .query_row(
            "SELECT task, workflow, question_to, concierge_json FROM tasks WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
}

/// The most recent task run under the `concierge` workflow: `forge ask`'s
/// own decision-making task, whose log carries the prompt it was given.
fn concierge_task_id(e: &Env) -> i64 {
    e.db()
        .query_row(
            "SELECT id FROM tasks WHERE workflow='concierge' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// The prompt the concierge directive was given: the log's first line is
/// always `{"type":"forge_prompt","text":...}` (see src/agent.rs).
fn concierge_prompt_text(e: &Env, id: i64) -> String {
    let log = e.log_text(id, 1);
    let first = log.lines().next().unwrap();
    let v: serde_json::Value = serde_json::from_str(first).unwrap();
    v["text"].as_str().unwrap().to_string()
}

/// A project named "demo" on `e.repo`, with a purpose, a backlog item, a
/// deploy target, a plain task on record, and a confirmed intake brief —
/// everything the concierge directive is given (see docs/INTAKE.md).
fn setup(e: &Env) {
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "demo",
                "--purpose",
                "Demo runs a small repair shop over text messages.",
                "--repo",
                repo,
            ],
        )
        .status
        .success()
    );
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "backlog",
                "demo",
                "--add",
                "text a reminder the day before a pickup",
            ],
        )
        .status
        .success()
    );
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "deploy",
                "add",
                "demo",
                "prod",
                "--repo",
                repo,
                "--method",
                "deploy-command",
                "--arg",
                "host=shop-laptop",
                "--check",
                "true",
            ],
        )
        .status
        .success()
    );
    assert!(
        e.forge(
            "ok.sh",
            &[
                "add",
                repo,
                "text the Hendersons a pickup reminder",
                "--project",
                "demo",
            ],
        )
        .status
        .success()
    );

    // A confirmed intake brief on the project's record.
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            repo,
            "Nate runs a shop. Contact: nate.",
            "--workflow",
            "intake",
            "--project",
            "demo",
            "--no-land",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        e.forge("interviewer-confirmed.sh", &["work", "--once"])
            .status
            .success()
    );
    let o = e.forge(
        "ok.sh",
        &[
            "answer",
            &id.to_string(),
            "Yes, that's right.",
            "--by",
            "nate",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        e.forge("interviewer-confirmed.sh", &["work", "--once"])
            .status
            .success()
    );
}

#[test]
fn a_request_files_a_task_on_the_projects_default_workflow_and_records_the_decision() {
    let e = Env::new();
    setup(&e);
    let o = e.forge(
        "concierge-request.sh",
        &[
            "ask",
            "demo",
            "Please change the quote text to say usually same day.",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    let filed: i64 = out
        .lines()
        .find_map(|l| l.strip_prefix("concierge: a request; filed task "))
        .unwrap_or_else(|| panic!("{out}"))
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();

    let (state, _, _) = e.task(filed);
    assert_eq!(state, "queued");
    let (task, workflow, _, concierge_json) = row(&e, filed);
    assert!(task.contains("usually same day"), "{task}");
    assert_eq!(workflow, "direct", "the project's default workflow");
    let d: serde_json::Value =
        serde_json::from_str(&concierge_json.expect("concierge_json is recorded")).unwrap();
    assert_eq!(d["kind"], "request");

    // The prompt carried the project's own record.
    let cid = concierge_task_id(&e);
    let prompt = concierge_prompt_text(&e, cid);
    assert!(prompt.contains("Demo runs a small repair shop"), "{prompt}");
    assert!(
        prompt.contains("text a reminder the day before a pickup"),
        "{prompt}"
    );
    assert!(prompt.contains("shop-laptop"), "{prompt}");
    assert!(
        prompt.contains("text the Hendersons a pickup reminder"),
        "{prompt}"
    );
    assert!(prompt.contains("quote by photo"), "{prompt}");
}

#[test]
fn a_question_prints_the_answer_and_records_a_decision_not_a_task() {
    let e = Env::new();
    setup(&e);
    let before: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();

    let o = e.forge(
        "concierge-question.sh",
        &[
            "ask",
            "demo",
            "Did the reminder go out to the Hendersons?",
            "--from",
            "nate",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert_eq!(
        out,
        "Yes, the reminder task for the Hendersons landed this morning."
    );

    // No task was filed for a question: only the concierge's own run.
    let after: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, before + 1, "a question files no task of its own");

    let ds: serde_json::Value = e.decisions_json();
    let ds = ds.as_array().unwrap();
    let d = ds
        .iter()
        .find(|d| d["answered_by"] == "concierge")
        .unwrap_or_else(|| panic!("{ds:?}"));
    assert_eq!(d["question"], "Did the reminder go out to the Hendersons?");
    assert_eq!(
        d["answer"],
        "Yes, the reminder task for the Hendersons landed this morning."
    );
    assert_eq!(d["answered_for"], "nate");
}

#[test]
fn a_short_decision_is_not_held_to_plan_substantive() {
    let e = Env::new();
    setup(&e);
    let before: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();

    let o = e.forge(
        "concierge-short.sh",
        &[
            "ask",
            "demo",
            "Did the reminder go out to the Hendersons?",
            "--from",
            "nate",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert_eq!(out, "No.");

    // No task was filed for a question: only the concierge's own run.
    let after: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, before + 1, "a question files no task of its own");

    let ds: serde_json::Value = e.decisions_json();
    let ds = ds.as_array().unwrap();
    let d = ds
        .iter()
        .find(|d| d["answered_by"] == "concierge")
        .unwrap_or_else(|| panic!("{ds:?}"));
    assert_eq!(d["answer"], "No.");
    assert_eq!(d["answered_for"], "nate");
}

#[test]
fn a_need_files_an_intake_task_naming_the_contact() {
    let e = Env::new();
    setup(&e);
    let o = e.forge(
        "concierge-need.sh",
        &[
            "ask",
            "demo",
            "I keep losing track of who I've quoted.",
            "--from",
            "alice",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    let filed: i64 = out
        .lines()
        .find_map(|l| l.strip_prefix("concierge: a need"))
        .and_then(|l| l.rsplit(' ').next())
        .unwrap_or_else(|| panic!("{out}"))
        .parse()
        .unwrap();

    let (task, workflow, _, concierge_json) = row(&e, filed);
    assert_eq!(workflow, "intake");
    assert!(
        task.starts_with("I keep losing track of who I've quoted."),
        "{task}"
    );
    assert!(task.contains("Contact: alice."), "{task}");
    let d: serde_json::Value =
        serde_json::from_str(&concierge_json.expect("concierge_json is recorded")).unwrap();
    assert_eq!(d["kind"], "need");
}

#[test]
fn unclear_blocks_a_placeholder_task_with_the_question_addressed_to_the_contact() {
    let e = Env::new();
    setup(&e);
    let o = e.forge(
        "concierge-unclear.sh",
        &[
            "ask",
            "demo",
            "Can you fix the price on that one?",
            "--from",
            "alice",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    let blocked: i64 = out
        .lines()
        .find_map(|l| l.strip_prefix("concierge: unclear; blocked task "))
        .unwrap_or_else(|| panic!("{out}"))
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();

    let (state, reason, _) = e.task(blocked);
    assert_eq!(state, "blocked");
    assert!(
        reason.contains("Do you want the price change applied to future quotes only"),
        "{reason}"
    );
    let (_, _, question_to, concierge_json) = row(&e, blocked);
    assert_eq!(question_to.as_deref(), Some("alice"));
    assert!(concierge_json.is_some());

    let reqs: serde_json::Value = e.requests_json();
    let r = reqs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == blocked)
        .unwrap_or_else(|| panic!("{reqs}"));
    assert_eq!(r["kind"], "question");
    assert_eq!(r["to"], "alice");
}
