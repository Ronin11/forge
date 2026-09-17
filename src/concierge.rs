//! `forge ask`: the front door, not the interview (see docs/INTAKE.md,
//! "The front door is not the interview"). A customer's message is a
//! `request`, a `question`, a `need`, or `unclear`; the `concierge`
//! directive, given the project's purpose, brief, backlog, deploy
//! targets and last twenty tasks, sorts it into one of those and this
//! module acts on the decision: files the task, prints the answer, files
//! an intake task, or blocks a small placeholder task with the question
//! addressed to the contact. Every decision is recorded: a
//! `concierge_json` column on the task it produced, or a `decisions` row
//! for an answer.

use crate::ctx::Forge;
use crate::queue::{self, FileTask, TaskRequest};
use crate::store::{Initiative, TaskState};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

/// What the `concierge` directive returns in its `summary`: which of the
/// four kinds, and the one field that kind needs. Everything else is
/// left empty and ignored.
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
struct Decision {
    kind: String,
    /// `request`: the task text to file.
    task: String,
    /// `question`: the answer, in plain words.
    answer: String,
    /// `need`: one sentence saying why an interview is warranted.
    reason: String,
    /// `unclear`: the one question to ask.
    question: String,
    /// The escalator (docs/INTAKE.md, "The escalator"): set alongside
    /// whichever kind above when three or more of the project's recent
    /// requests are the same shape, in the directive's judgment.
    pattern: Option<Pattern>,
}

/// The escalator's pattern, as the concierge directive names it: the
/// quoted requests that share a shape, why (one sentence), and what the
/// automation it proposes would do (one sentence, the initiative's
/// outcome if the operator says yes).
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(default)]
struct Pattern {
    task_ids: Vec<i64>,
    repetition: String,
    outcome: String,
}

/// What `forge ask` did with the message, for the CLI to report.
pub enum Asked {
    Filed { task: i64 },
    Answered { answer: String, decision: i64 },
    Need { task: i64, reason: String },
    Unclear { task: i64, question: String },
}

/// A `TaskRequest` with the CLI's own defaults (`TaskArgs`'s
/// `default_value_t`s), the only fields every branch below needs to fill
/// in: the repo, the text, the project, and which workflow runs it.
/// `workflow: None` leaves it to `enqueue`'s own resolution — the
/// project's default, else "direct" — which is what "the project's
/// default workflow" means for a filed request.
fn base(project: &str, repo: &str, task: String, workflow: Option<&str>) -> TaskRequest {
    TaskRequest {
        repo: PathBuf::from(repo),
        task,
        project: Some(project.to_string()),
        workflow: workflow.map(str::to_string),
        max_turns: 100,
        retries: 1,
        timeout_secs: 1800,
        ..Default::default()
    }
}

/// Run the concierge on `message` and act on its decision. `f` is an
/// `Arc` because reaching the decision means running the `concierge`
/// workflow to completion, the same way `forge run` drives a task. The
/// second element is the escalator's proposal placeholder, filed
/// alongside whichever `Asked` the message itself decided, when the
/// decision named a `pattern` (see docs/INTAKE.md, "The escalator").
pub async fn ask(
    f: Arc<Forge>,
    project: &str,
    message: &str,
    from: Option<&str>,
) -> Result<(Asked, Option<i64>)> {
    let repo = f
        .store
        .first_repo(project)?
        .with_context(|| format!("project {project} lists no repository"))?;

    let mut req = base(project, &repo, message.to_string(), Some("concierge"));
    req.retries = 0;
    req.no_land = true;
    let t = queue::enqueue(&f, &req, None).await?;
    if !f.store.claim(t.id, std::process::id() as i64)? {
        bail!(
            "task {} was claimed by another worker before the concierge could run it",
            t.id
        );
    }
    let state = crate::worker::drive(f.clone(), t.id).await?;
    let t = f
        .store
        .task(t.id)?
        .with_context(|| format!("no task {}", t.id))?;
    if state != TaskState::Succeeded {
        bail!(
            "the concierge did not reach a decision (task {} is {}): {}",
            t.id,
            state.as_str(),
            t.reason
        );
    }
    if t.plan.is_empty() {
        bail!("the concierge task {} recorded no decision", t.id);
    }
    let raw = t.plan.clone();
    let d: Decision = serde_json::from_str(&raw).with_context(|| {
        format!(
            "task {}'s decision does not fit the concierge's schema: {raw}",
            t.id
        )
    })?;

    let asked = match d.kind.as_str() {
        "request" => {
            if d.task.trim().is_empty() {
                bail!("the concierge called this a request but named no task text");
            }
            // An ordinary task on the project's own default workflow: it
            // lands like any other once verified.
            let req = base(project, &repo, d.task.clone(), None);
            let mut n = queue::enqueue(&f, &req, None).await?;
            n.concierge_json = Some(raw.clone());
            f.store.update_task(&n)?;
            Asked::Filed { task: n.id }
        }
        "question" => {
            if d.answer.trim().is_empty() {
                bail!("the concierge called this a question but gave no answer");
            }
            let decision = f.store.insert_decision_by(
                t.id,
                &repo,
                message,
                &d.answer,
                "concierge",
                "",
                from,
            )?;
            Asked::Answered {
                answer: d.answer.clone(),
                decision,
            }
        }
        "need" => {
            // "Contact: X." is the same convention an intake task's text
            // already carries when a person is named (see
            // tests/e2e/intake.rs); the interview reads the contact back
            // out of the task text the same way.
            let text = match from {
                Some(c) => format!("{message} Contact: {c}."),
                None => message.to_string(),
            };
            let mut req = base(project, &repo, text, Some("intake"));
            req.retries = 0;
            req.no_land = true;
            let mut n = queue::enqueue(&f, &req, None).await?;
            n.concierge_json = Some(raw.clone());
            f.store.update_task(&n)?;
            Asked::Need {
                task: n.id,
                reason: d.reason.clone(),
            }
        }
        "unclear" => {
            if d.question.trim().is_empty() {
                bail!("the concierge called this unclear but asked no question");
            }
            let mut req = base(project, &repo, message.to_string(), Some("direct"));
            req.retries = 0;
            req.no_land = true;
            let mut n = queue::enqueue(&f, &req, None).await?;
            n.state = TaskState::Blocked;
            n.reason = format!("needs input: {}", d.question);
            n.question_to = from.map(str::to_string);
            n.concierge_json = Some(raw.clone());
            f.store.update_task(&n)?;
            Asked::Unclear {
                task: n.id,
                question: d.question.clone(),
            }
        }
        other => bail!("the concierge returned an unknown decision kind {other:?}"),
    };

    let proposal = match &d.pattern {
        Some(p) if is_a_pattern(p) => Some(file_proposal(&f, project, &repo, p, &raw, from).await?),
        _ => None,
    };

    Ok((asked, proposal))
}

/// Whether the directive's `pattern` field actually names one: three or
/// more quoted requests, a repetition and an outcome, both non-empty. A
/// directive that leaves `pattern` present but hollow (an empty array, an
/// empty sentence) is treated as not having found one, rather than
/// failing the whole decision over an optional field.
fn is_a_pattern(p: &Pattern) -> bool {
    p.task_ids.len() >= 3 && !p.repetition.trim().is_empty() && !p.outcome.trim().is_empty()
}

/// The escalator (docs/INTAKE.md, "The escalator"): blocks a placeholder
/// task with one question, addressed to the contact, proposing the
/// automation the pattern names and asking yes or no; records the
/// proposal on the placeholder's `proposal_json` so `forge answer` can
/// find it again. Returns the placeholder task's id.
async fn file_proposal(
    f: &Forge,
    project: &str,
    repo: &str,
    p: &Pattern,
    raw: &str,
    from: Option<&str>,
) -> Result<i64> {
    let question = format!(
        "{} Want it: {}? (yes or no)",
        p.repetition.trim(),
        p.outcome.trim()
    );
    let mut req = base(project, repo, p.outcome.clone(), Some("direct"));
    req.retries = 0;
    req.no_land = true;
    let mut n = queue::enqueue(f, &req, None).await?;
    n.state = TaskState::Blocked;
    n.reason = format!("needs input: {question}");
    n.question_to = from.map(str::to_string);
    n.concierge_json = Some(raw.to_string());
    n.proposal_json = Some(serde_json::to_string(&crate::view::ProposalRecord {
        task_ids: p.task_ids.clone(),
        repetition: p.repetition.clone(),
        outcome: p.outcome.clone(),
    })?);
    f.store.update_task(&n)?;
    Ok(n.id)
}

/// What answering an escalator proposal did: a "yes" filed an initiative
/// (its id, and the tasks filed for it, one per quoted request's shape);
/// a "no" just recorded the decision.
pub enum ProposalAnswered {
    Initiative { initiative: i64, tasks: Vec<i64> },
    Declined,
}

/// Answer an escalator's proposal placeholder (see `file_proposal`):
/// records the decision the same way any other answer does, then, on a
/// yes, files an initiative on the project with the pattern's outcome and
/// one task per quoted request's shape (the same machinery `forge
/// initiative new --from` files a hand-written one with; here the
/// paragraphs are the quoted requests' own texts, generated rather than
/// read from a file). Unlike `queue::answer`, this placeholder never ran
/// an agent turn, so there is no attempt to retry: the placeholder itself
/// is marked settled instead.
pub async fn answer_proposal(f: &Forge, id: i64, text: &str, by: &str) -> Result<ProposalAnswered> {
    let mut t = f.store.task(id)?.with_context(|| format!("no task {id}"))?;
    if t.state != TaskState::Blocked {
        bail!(
            "task {id} is {}; only a blocked proposal is answered this way",
            t.state.as_str()
        );
    }
    let raw = t
        .proposal_json
        .clone()
        .with_context(|| format!("task {id} carries no escalator proposal"))?;
    let p: crate::view::ProposalRecord = serde_json::from_str(&raw)
        .with_context(|| format!("task {id}'s proposal does not fit its own schema: {raw}"))?;
    let (_, question) = crate::view::request_kind(&t.reason);
    f.store.insert_decision_by(
        id,
        &t.repo,
        &question,
        text,
        by,
        "",
        t.question_to.as_deref(),
    )?;

    let yes = is_yes(text);
    let result = if yes {
        let project = t
            .project
            .clone()
            .with_context(|| format!("task {id} has no project"))?;
        let ini_id = f.store.create_initiative(&Initiative {
            project: project.clone(),
            outcome: p.outcome.clone(),
            stop_after_same_rule: 3,
            created_at: crate::unix_now(),
            ..Default::default()
        })?;
        let mut paragraphs: Vec<FileTask> = Vec::new();
        for qid in &p.task_ids {
            let qt = f
                .store
                .task(*qid)?
                .with_context(|| format!("proposal quotes task {qid} which no longer exists"))?;
            paragraphs.push(FileTask {
                after: None,
                repo: Some(qt.repo.clone()),
                provider: None,
                workflow: None,
                text: qt.task.clone(),
            });
        }
        let ids = queue::file_initiative_paragraphs(
            f,
            &project,
            ini_id,
            &paragraphs,
            Some(&t.repo),
            None,
            None,
        )
        .await?;
        t.proposal_answer = Some("yes".to_string());
        t.proposal_initiative = Some(ini_id);
        t.state = TaskState::Succeeded;
        t.reason = format!("proposal accepted: initiative {ini_id}");
        ProposalAnswered::Initiative {
            initiative: ini_id,
            tasks: ids,
        }
    } else {
        t.proposal_answer = Some("no".to_string());
        t.state = TaskState::Succeeded;
        t.reason = "proposal declined".to_string();
        ProposalAnswered::Declined
    };
    f.store.update_task(&t)?;
    Ok(result)
}

/// A plain yes, tolerant of a trailing "please", punctuation or a leading
/// "y" — never a guess when the text is actually ambiguous, which reads
/// as no more than the operator not having said yes.
fn is_yes(text: &str) -> bool {
    let t = text.trim().trim_end_matches(['.', '!']).to_lowercase();
    t == "y" || t == "yes" || t.starts_with("yes,") || t.starts_with("yes ")
}
