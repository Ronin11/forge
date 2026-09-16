//! Intake step 2 (docs/INTAKE.md): the `interview` directive and the
//! `intake` workflow. A scripted fake person (tests/fakes/interviewer.sh)
//! answers from a fixture over four blocking turns and confirms; a second
//! fixture (tests/fakes/interviewer-stop.sh) says they want to stop.

use crate::support::*;

fn add_intake(e: &Env, task: &str) -> i64 {
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            task,
            "--workflow",
            "intake",
            "--no-land",
            "--retries",
            "0",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap()
}

fn answer(e: &Env, id: i64, text: &str, by: &str) -> i64 {
    let o = e.forge("ok.sh", &["answer", &id.to_string(), text, "--by", by]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(4)
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn a_scripted_person_answers_four_questions_and_confirms_the_brief() {
    let e = Env::new();
    let mut id = add_intake(&e, "Nate runs a shop. Contact: nate.");

    let answers = [
        "A photo of a finished job.",
        "I text back a quote and write it in the book.",
        "Yes, that is right.",
        "Yes, that is right.",
    ];
    let mut questions = Vec::new();
    for (turn, ans) in answers.iter().enumerate() {
        let o = e.forge("interviewer.sh", &["work", "--once"]);
        assert!(
            o.status.success(),
            "turn {turn}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let (state, reason, _) = e.task(id);
        assert_eq!(state, "blocked", "turn {turn}: {reason}");
        let question = reason
            .strip_prefix("needs input: ")
            .unwrap_or(&reason)
            .to_string();
        assert!(
            question.matches('?').count() <= 1,
            "turn {turn} asked more than one question: {question:?}"
        );
        let to = e.requests_json()[0]["to"].as_str().map(str::to_string);
        assert_eq!(
            to.as_deref(),
            Some("nate"),
            "turn {turn}: the question must be addressed to the contact"
        );
        questions.push(question);
        id = answer(&e, id, ans, "nate");
    }
    assert_eq!(questions.len(), 4);

    // The confirming turn (the one before the last answer) wrote the brief
    // as its plan alongside the confirmation question.
    let confirming_id = id - 1;
    let doc = e.trace_json(confirming_id);
    let plan = doc["task"]["plan"]
        .as_str()
        .expect("the confirming turn recorded a plan");
    let brief: serde_json::Value =
        serde_json::from_str(plan).unwrap_or_else(|e| panic!("plan is not JSON: {e}: {plan}"));
    let workflows = brief["workflows"]
        .as_array()
        .expect("brief.workflows is an array");
    assert!(!workflows.is_empty(), "at least one workflow was named");
    for w in workflows {
        for field in [
            "name",
            "trigger",
            "inputs",
            "outputs",
            "other_people",
            "failure_today",
            "success_signal",
            "do_not_touch",
        ] {
            assert!(
                w.get(field).is_some_and(|v| v.is_string()),
                "workflow entry missing `{field}`: {w}"
            );
        }
    }
    assert!(brief["where_it_runs"].is_string(), "{brief}");
    assert!(brief["do_not_touch"].is_array(), "{brief}");
    assert_eq!(brief["confirmed"], false, "{brief}");

    // The person's final "yes" ends the interview without another question.
    let o = e.forge("interviewer.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(id);
    assert_eq!(state, "succeeded", "{reason}");
}

#[test]
fn a_person_saying_stop_ends_the_interview_without_a_plan_substantive_failure() {
    let e = Env::new();
    let id = add_intake(&e, "Nate runs a shop. Contact: nate.");

    let o = e.forge("interviewer-stop.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(id);
    assert_ne!(
        state, "failed",
        "honoring a stop request must not fail the task: {reason}"
    );
    assert_eq!(state, "succeeded", "{reason}");

    let doc = e.trace_json(id);
    let failing_rules: Vec<String> = doc["attempts"][0]["verdict"]
        .as_array()
        .expect("a verdict array")
        .iter()
        .filter(|c| !c["ok"].as_bool().unwrap_or(true))
        .map(|c| c["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !failing_rules.contains(&"plan-substantive".to_string()),
        "the interview's stop message must not be judged as a repository plan: {failing_rules:?}"
    );
    assert!(
        !failing_rules.contains(&"plan-names-real-paths".to_string()),
        "{failing_rules:?}"
    );
}
