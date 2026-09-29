use super::*;
use crate::store::InsertDecisionBy;

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db")).unwrap();
    (dir, store)
}

fn task(store: &Store, state: TaskState, parent: Option<i64>) -> Task {
    let mut t = Task {
        state,
        retry_of: parent,
        workflow: "direct".into(),
        repo: "/repo".into(),
        ..Default::default()
    };
    t.id = store.insert_task(&t).unwrap();
    store.update_task(&t).unwrap();
    t
}

fn decision(store: &Store, t: &Task, kind: &str, next: Option<i64>) {
    let id = store
        .insert_decision_by(InsertDecisionBy {
            task_id: t.id,
            repo: &t.repo,
            question: "question",
            answer: "answer",
            answered_by: "supervisor",
            citations: "",
            answered_for: None,
        })
        .unwrap();
    store.set_decision_kind(id, kind).unwrap();
    if let Some(next) = next {
        store.set_decision_retry(id, next).unwrap();
    }
}

#[test]
fn operator_contact_and_job_questions_ask_a_person() {
    let (_dir, s) = fixture();
    let mut t = task(&s, TaskState::Blocked, None);
    for to in [None, Some("operator"), Some("alice")] {
        t.question_to = to.map(str::to_string);
        s.update_task(&t).unwrap();
        assert_eq!(classify(&s, &t).unwrap(), "person");
    }
    t.task = "job question".into();
    s.update_task(&t).unwrap();
    assert_eq!(classify(&s, &t).unwrap(), "person");
}

#[test]
fn a_demotion_is_handled_only_after_its_followup_is_filed() {
    let (_dir, s) = fixture();
    let t = task(&s, TaskState::Blocked, None);
    decision(&s, &t, "demotion-as-task", None);
    assert_eq!(classify(&s, &t).unwrap(), "person");
    let next = task(&s, TaskState::Queued, Some(t.id));
    decision(&s, &t, "demotion-as-task", Some(next.id));
    assert_eq!(classify(&s, &t).unwrap(), "none");
}

#[test]
fn failures_with_retries_or_refiles_are_handled_and_other_failures_ask() {
    let (_dir, s) = fixture();
    let t = task(&s, TaskState::Failed, None);
    assert_eq!(classify(&s, &t).unwrap(), "person");
    let retry = task(&s, TaskState::Queued, Some(t.id));
    assert_eq!(classify(&s, &t).unwrap(), "none");
    let refiled = task(&s, TaskState::Failed, None);
    decision(&s, &refiled, "mechanic-ratchet", Some(retry.id));
    assert_eq!(classify(&s, &refiled).unwrap(), "none");
}

#[test]
fn a_dependency_without_a_live_followup_asks_and_a_superseded_block_does_not() {
    let (_dir, s) = fixture();
    let parent = task(&s, TaskState::Failed, None);
    let mut dependent = task(&s, TaskState::Blocked, None);
    dependent.after = vec![parent.id];
    dependent.reason = format!("waits on task {}", parent.id);
    s.update_task(&dependent).unwrap();
    assert_eq!(classify(&s, &dependent).unwrap(), "person");
    let mut retry = task(&s, TaskState::Queued, Some(parent.id));
    decision(&s, &parent, "mechanic-load-flake", Some(retry.id));
    assert_eq!(classify(&s, &dependent).unwrap(), "none");
    retry.state = TaskState::Failed;
    s.update_task(&retry).unwrap();
    assert_eq!(classify(&s, &dependent).unwrap(), "person");
    task(&s, TaskState::Running, Some(dependent.id));
    assert_eq!(classify(&s, &dependent).unwrap(), "none");
}

#[test]
fn successful_transitions_do_not_ask_a_person() {
    let (_dir, s) = fixture();
    let t = task(&s, TaskState::Succeeded, None);
    assert_eq!(classify(&s, &t).unwrap(), "none");
}
