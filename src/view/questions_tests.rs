use super::*;
use crate::store::RetryFacts;

const DEMOTION: &str = "the fix is off by one: `cargo test parse::edge` fails with left 3, right 4";

#[test]
fn an_answer_that_only_restates_the_ask_is_as_stated() {
    let q = "Should I rename foo to bar in src/lib.rs?";
    for a in [
        "Do it as stated.",
        "yes",
        "Yes, do it",
        "Go ahead",
        "Proceed as written",
        "Rename foo to bar in src/lib.rs.",
    ] {
        assert!(answer_repeats_ask(q, a), "{a}");
    }
}

#[test]
fn an_answer_that_adds_a_decision_is_not_as_stated() {
    let q = "Which answer file: answer.txt or ANSWER.txt?";
    for a in [
        "Use answer.txt",
        "keep the branch, make this one fix",
        "No, don't rename it",
        "no",
        "",
        "Rename it to baz and update the docs",
    ] {
        assert!(!answer_repeats_ask(q, a), "{a}");
    }
}

#[test]
fn the_landed_retry_with_only_the_answer_appended_is_as_stated() {
    let parent = "fix the parser";
    let answer = "keep the branch, make this one fix";
    let operator =
        format!("{parent}\n\nOperator's answer to a question from an earlier attempt: {answer}");
    let supervisor = format!(
        "{parent}\n\nSupervisor's answer to a question from an earlier attempt (citing task 3, src/a.rs): {answer}"
    );
    assert!(landed_with_answer_appended(parent, &operator, true, answer));
    assert!(landed_with_answer_appended(
        parent,
        &supervisor,
        true,
        answer
    ));
    assert!(!landed_with_answer_appended(
        parent, &operator, false, answer
    ));
}

#[test]
fn a_retry_that_changed_anything_else_is_not_as_stated() {
    let parent = "fix the parser";
    let answer = "make this one fix";
    let edited = format!(
        "fix the parser and the lexer\n\nOperator's answer to a question from an earlier attempt: {answer}"
    );
    let extra = format!(
        "{parent}\n\nOperator's answer to a question from an earlier attempt: {answer}\n\nAlso bump the version."
    );
    let other = format!(
        "{parent}\n\nOperator's answer to a question from an earlier attempt: something else"
    );
    for retry in [edited, extra, other] {
        assert!(
            !landed_with_answer_appended(parent, &retry, true, answer),
            "{retry}"
        );
    }
}

fn demotion(id: i64, by_supervisor: bool, landed: bool) -> QuestionRecord {
    let answer = "keep the branch, make this one fix";
    let parent = format!("task {id}");
    QuestionRecord {
        kind: "review",
        blocked_at: 0,
        question: DEMOTION.into(),
        task: parent.clone(),
        resolution: if by_supervisor {
            Resolution::Supervisor { at: 3600 }
        } else {
            Resolution::Operator { at: 3600 }
        },
        answer: Some(answer.into()),
        retry: Some(RetryFacts {
            task: format!(
                "{parent}\n\nSupervisor's answer to a question from an earlier attempt (citing task 1): {answer}"
            ),
            landed,
        }),
    }
}

#[test]
fn a_demotion_answered_with_one_fix_that_landed_is_as_stated() {
    assert!(is_as_stated(&demotion(640, true, true)));
    assert!(!is_as_stated(&demotion(640, true, false)));
}

#[test]
fn withdrawn_and_open_questions_are_never_as_stated() {
    let mut w = demotion(1, false, true);
    w.resolution = Resolution::Withdrawn { at: 10 };
    assert!(!is_as_stated(&w));
    let mut o = demotion(2, false, true);
    o.resolution = Resolution::Open;
    assert!(!is_as_stated(&o));
}

#[test]
fn the_document_counts_kinds_medians_and_prices_operator_attention() {
    let mut open = demotion(4, false, false);
    open.kind = "question";
    open.resolution = Resolution::Open;
    open.blocked_at = 0;
    let mut withdrawn = demotion(5, false, false);
    withdrawn.kind = "question";
    withdrawn.resolution = Resolution::Withdrawn { at: 7200 };
    let records = vec![
        demotion(1, true, true),
        demotion(2, true, true),
        demotion(3, false, true),
        open,
        withdrawn,
    ];
    let doc = questions_from_records(&records, Some(7), 10_800, Some(120.0), 5.0);
    let review = &doc.kinds[0];
    assert_eq!(review.kind, "review");
    assert_eq!(review.count, 3);
    assert_eq!(review.answered_by_supervisor, 2);
    assert_eq!(review.answered_by_operator, 1);
    assert_eq!(review.as_stated, 3);
    assert_eq!(review.median_wait_hours, Some(1.0));
    let plain = &doc.kinds[1];
    assert_eq!((plain.count, plain.open, plain.withdrawn), (2, 1, 1));
    assert_eq!(plain.median_wait_hours, Some(2.5));
    assert_eq!(doc.total.count, 5);
    assert_eq!(doc.total.operator_handled, 2);
    let cost = doc.total.attention_cost_usd.unwrap();
    assert!((cost - 2.0 * 5.0 / 60.0 * 120.0).abs() < 1e-9, "{cost}");
    let unpriced = questions_from_records(&records, None, 10_800, None, 5.0);
    assert_eq!(unpriced.total.attention_cost_usd, None);
    assert_eq!(unpriced.kinds[2].median_wait_hours, None);
}
