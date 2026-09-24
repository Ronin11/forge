use crate::ctx::Forge;
use crate::store::{QuestionRecord, Resolution};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeSet;

/// The kinds of question that block a task, in the order the table lists
/// them.
pub const QUESTION_KINDS: [&str; 5] = ["review", "question", "workflow", "job", "dependency"];

const AS_STATED_PHRASES: [&str; 8] = [
    "as stated",
    "as written",
    "as specified",
    "as described",
    "as asked",
    "as requested",
    "go ahead",
    "proceed",
];

const STOPWORDS: [&str; 24] = [
    "a", "an", "the", "to", "of", "and", "or", "it", "is", "do", "this", "that", "please", "yes",
    "in", "on", "for", "with", "so", "then", "just", "you", "i", "be",
];

const APPENDED_MARKER: &str = "answer to a question from an earlier attempt";

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// True when an answer's text only repeats what the question itself asked
/// for: an explicit "as stated"/"go ahead", a bare "yes"/"do it", or at
/// least two content words of which at least four in five already appear
/// in the question ("Rename foo to bar" answering "Should I rename foo to
/// bar?"). An answer that adds words the question never used is a
/// decision, not a repeat.
pub fn answer_repeats_ask(question: &str, answer: &str) -> bool {
    let a = words(answer);
    let joined = a.join(" ");
    if joined == "yes" || joined == "do it" || joined == "yes do it" {
        return true;
    }
    if AS_STATED_PHRASES.iter().any(|p| joined.contains(p)) {
        return true;
    }
    let asked: BTreeSet<String> = words(question).into_iter().collect();
    let content: Vec<&String> = a
        .iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect();
    if content.len() < 2 {
        return false;
    }
    let shared = content.iter().filter(|w| asked.contains(**w)).count();
    shared * 5 >= content.len() * 4
}

/// True when the task an answer re-queued is the parent's own text plus the
/// answer appended and nothing else, and it landed: the lineage's next
/// task took the answer as the whole change to the request.
pub fn landed_with_answer_appended(
    parent_task: &str,
    retry_task: &str,
    retry_landed: bool,
    answer: &str,
) -> bool {
    if !retry_landed {
        return false;
    }
    let Some(rest) = retry_task
        .strip_prefix(parent_task)
        .and_then(|r| r.strip_prefix("\n\n"))
    else {
        return false;
    };
    let Some(at) = rest.find(APPENDED_MARKER) else {
        return false;
    };
    if rest[..at].contains('\n') {
        return false;
    }
    let after = &rest[at + APPENDED_MARKER.len()..];
    if let Some(tail) = after.strip_prefix(": ") {
        tail == answer
    } else if after.starts_with(" (citing ") {
        after.find("): ").is_some_and(|i| &after[i + 3..] == answer)
    } else {
        false
    }
}

/// Whether an answered question was "do it as stated": either the answer
/// repeats the ask, or the lineage's next task landed carrying the answer
/// and nothing else. Withdrawn and open questions are never as stated.
pub fn is_as_stated(r: &QuestionRecord) -> bool {
    if !matches!(
        r.resolution,
        Resolution::Supervisor { .. } | Resolution::Operator { .. }
    ) {
        return false;
    }
    let Some(answer) = r.answer.as_deref() else {
        return false;
    };
    answer_repeats_ask(&r.question, answer)
        || r.retry
            .as_ref()
            .is_some_and(|c| landed_with_answer_appended(&r.task, &c.task, c.landed, answer))
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// One row of `QuestionsDoc.kinds` (and its `total`): the questions of one
/// kind that blocked a task in the window.
#[derive(Serialize, Debug, Default, PartialEq)]
pub struct QuestionKindRow {
    /// `review`, `question`, `workflow`, `job`, `dependency`, or `all`.
    pub kind: String,
    pub count: i64,
    /// Still blocked, waiting on a person.
    pub open: i64,
    pub answered_by_supervisor: i64,
    pub answered_by_operator: i64,
    pub withdrawn: i64,
    /// Answered ones that were "do it as stated" (see `is_as_stated`).
    pub as_stated: i64,
    /// Median hours from blocking to being settled; a question still open
    /// counts as having waited until now. `None` when there are none.
    pub median_wait_hours: Option<f64>,
    /// Questions a person handled: operator answers and withdrawals.
    pub operator_handled: i64,
    /// Attention hours those took, at the configured minutes each.
    pub attention_hours: f64,
    /// Those hours at the configured hourly rate; `None` when no rate is
    /// configured.
    pub attention_cost_usd: Option<f64>,
}

/// `forge stats --questions --json`: every task that blocked with a
/// question in the window, by kind.
#[derive(Serialize, Debug, PartialEq)]
pub struct QuestionsDoc {
    /// The window in days; `None` is all time.
    pub days: Option<i64>,
    pub operator_usd_per_hour: Option<f64>,
    pub attention_minutes_per_question: f64,
    pub kinds: Vec<QuestionKindRow>,
    pub total: QuestionKindRow,
}

fn row(
    kind: &str,
    records: &[&QuestionRecord],
    now: i64,
    rate: Option<f64>,
    minutes: f64,
) -> QuestionKindRow {
    let mut r = QuestionKindRow {
        kind: kind.to_string(),
        count: records.len() as i64,
        ..Default::default()
    };
    let mut waits = Vec::new();
    for q in records {
        let end = match q.resolution {
            Resolution::Open => {
                r.open += 1;
                now
            }
            Resolution::Supervisor { at } => {
                r.answered_by_supervisor += 1;
                at
            }
            Resolution::Operator { at } => {
                r.answered_by_operator += 1;
                at
            }
            Resolution::Withdrawn { at } => {
                r.withdrawn += 1;
                at
            }
        };
        if is_as_stated(q) {
            r.as_stated += 1;
        }
        waits.push((end - q.blocked_at).max(0) as f64 / 3600.0);
    }
    r.median_wait_hours = median(waits);
    r.operator_handled = r.answered_by_operator + r.withdrawn;
    r.attention_hours = r.operator_handled as f64 * minutes / 60.0;
    r.attention_cost_usd = rate.map(|usd| r.attention_hours * usd);
    r
}

/// Fold question records into the document; pure, so tests fix `now`.
pub fn questions_from_records(
    records: &[QuestionRecord],
    days: Option<i64>,
    now: i64,
    rate: Option<f64>,
    minutes: f64,
) -> QuestionsDoc {
    let kinds = QUESTION_KINDS
        .iter()
        .map(|k| {
            let of: Vec<&QuestionRecord> = records.iter().filter(|q| q.kind == *k).collect();
            row(k, &of, now, rate, minutes)
        })
        .collect();
    let all: Vec<&QuestionRecord> = records.iter().collect();
    QuestionsDoc {
        days,
        operator_usd_per_hour: rate,
        attention_minutes_per_question: minutes,
        kinds,
        total: row("all", &all, now, rate, minutes),
    }
}

pub fn questions_doc(f: &Forge, days: Option<i64>) -> Result<QuestionsDoc> {
    let now = crate::unix_now();
    let since = days.map(|d| now - d * 86_400);
    let records = f.store.question_records(since)?;
    Ok(questions_from_records(
        &records,
        days,
        now,
        f.measure.operator_usd_per_hour,
        f.measure.attention_minutes_per_question,
    ))
}

#[cfg(test)]
#[path = "questions_tests.rs"]
mod questions_tests;
