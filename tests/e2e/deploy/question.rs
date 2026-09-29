use super::*;

pub(super) fn land_task(e: &Env) -> (String, String, bool) {
    let o = e.forge(
        "ok.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let landed = e.task(1);
    assert_eq!(landed.0, "succeeded");
    assert!(landed.1.starts_with("landed main @"), "{landed:?}");
    // A stale recipient on landed work must not become the deploy's recipient.
    e.db()
        .execute("UPDATE tasks SET question_to='alice' WHERE id=1", [])
        .unwrap();
    landed
}

pub(super) fn assert_answer_closes_question(
    e: &Env,
    landed: (String, String, bool),
    deploy_id: i64,
) {
    assert_eq!(e.task(1), landed, "rollback must preserve landed work");
    let (id, state, reason, to): (i64, String, String, Option<String>) = e
        .db()
        .query_row(
            "SELECT id, state, reason, question_to FROM tasks WHERE deploy_id=?1",
            [deploy_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_ne!(id, 1);
    assert_eq!(state, "blocked");
    assert_eq!(to, None, "the deploy asks the operator independently");
    assert!(reason.contains("rolled back to"), "{reason}");
    assert!(reason.contains("bad"), "{reason}");
    assert!(e.attempts(id).is_empty());

    let requests = e.forge("ok.sh", &["requests", "--json"]);
    assert!(requests.status.success());
    let requests: serde_json::Value = serde_json::from_slice(&requests.stdout).unwrap();
    let request = requests
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(
        request["kind"], "question",
        "the web inbox must offer an answer"
    );

    let o = e.forge("ok.sh", &["answer", &id.to_string(), "ok"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(id).0, "withdrawn");
    assert_eq!(e.task(1), landed);
    assert!(e.attempts(id).is_empty());
    let (question, answer): (String, String) = e
        .db()
        .query_row(
            "SELECT question, answer FROM decisions WHERE task_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(question, reason);
    assert_eq!(answer, format!("deploy {deploy_id} answered: ok"));
    let tasks: i64 = e
        .db()
        .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tasks, 2, "answering must not create a retry");
    let o = e.forge("ok.sh", &["answer", &id.to_string(), "again"]);
    assert!(!o.status.success(), "a question can only be answered once");
}
