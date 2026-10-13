//! Jev at the claim (docs/EXECUTION.md, "Admission and the judgment band";
//! docs/research/06-one-person-factories.md, 2.2). When a worker has just
//! claimed a task, Jev is asked once, in one request, up to two questions:
//!
//! 1. *Admission*, for tasks filed at contact or public trust only (the
//!    operator's own are admitted as they are): is this fit for a worker
//!    that cannot ask anyone anything? `[admission] threshold` or more
//!    admits; under it the task blocks with a question for the operator
//!    that quotes the probability and the gap Jev named. Task text
//!    matching `[admission] exclude` blocks without asking, and a rolling
//!    day admits at most `[admission] per_day`, counted from the
//!    decision rows so a re-run cannot admit more.
//! 2. *Band*: `judgment` or `mechanical`, recorded on the task as `band`
//!    and `band_p`. Nothing routes on it.
//!
//! Only a `jev` provider answers; a claude lane is never asked. With none
//! configured the whole step is off. A Jev error admits an operator task
//! and parks any other. Then, on a task that runs, a local size model
//! predicts the change's size and may route its workflow (`size`).

use crate::{
    agent::{self, Provider, Runner},
    ctx::Forge,
    report::Event,
    store::{Attempt, AttemptState, FinishAttempt, InsertDecisionBy, Task, TaskState, Trust},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::time::Duration;

/// The step name of the attempt row a block leaves so the operator's
/// question can be answered (`queue::answer`).
pub const STEP: &str = "admission";
/// Decision kinds: one row per question asked, `ADMIT` counting toward the
/// day's cap.
pub const ADMIT: &str = "admission-admit";
pub const BLOCK: &str = "admission-block";
pub const EXCLUDE: &str = "admission-exclude";
pub const PARK: &str = "admission-park";
pub const BAND: &str = "band";

const QUESTION: &str = "Admit this task for an automated worker now? Yes only if the described change is concrete, scoped to this repository, with a clear way to tell it is done, that a coding worker can finish without asking anyone anything and without a product, pricing, legal, security or customer-data decision.";
const GAP_QUESTION: &str = "If the task should not be admitted, what mainly stands in the way?";
const BAND_QUESTION: &str = "Does finishing this task need judgment (a design choice, an ambiguity to resolve, reasoning across modules) or is it mechanical (a rote, fully determined edit)?";
const TIMEOUT: Duration = Duration::from_secs(30);
/// How much of the task text Jev is shown.
const STATE_CHARS: usize = 6000;
const DAY_SECS: i64 = 86_400;

/// The jev provider asked, when the step is on: `[admission] provider`
/// when it names a jev one, else the first jev provider by name.
fn provider(f: &Forge) -> Option<&Provider> {
    if !f.admission.enabled {
        return None;
    }
    let jev = |p: &&Provider| p.runner == Runner::Jev;
    match &f.admission.provider {
        Some(name) => f.providers.get(name).filter(jev),
        None => f.providers.values().find(jev),
    }
}

/// Whether the admission question is this task's: filed below operator
/// trust and not asked already on its lineage (an answered block retries
/// as a new task, which is not asked again).
fn gated(f: &Forge, t: &Task) -> Result<bool> {
    if t.trust == Trust::Operator {
        return Ok(false);
    }
    Ok(!f
        .store
        .decisions_in_lineage(t.id)?
        .iter()
        .any(|d| d.kind.starts_with("admission-")))
}

fn admitted_today(f: &Forge) -> Result<usize> {
    Ok(f.store
        .decisions_of_kind_since(ADMIT, crate::unix_now() - DAY_SECS)?
        .len())
}

/// Whether the day's cap keeps `t` from being claimed: its admission
/// question is still to be asked and the cap is spent. The worker leaves
/// it queued, so it goes through when the day rolls on.
pub fn capped(f: &Forge, t: &Task) -> bool {
    provider(f).is_some()
        && gated(f, t).unwrap_or(false)
        && admitted_today(f).is_ok_and(|n| n >= f.admission.per_day as usize)
}

fn request(t: &Task, ask_admit: bool, ask_band: bool) -> Value {
    let mut questions = serde_json::Map::new();
    if ask_admit {
        questions.insert(
            "admit".into(),
            json!({"type": "choice", "instructions": QUESTION, "criteria": {
                "yes": "the task is concrete, scoped, verifiable and needs no decision from anyone",
                "no": "something in the task needs a person first",
            }}),
        );
        questions.insert(
            "gap".into(),
            json!({"type": "choice", "instructions": GAP_QUESTION, "criteria": {
                "none": "nothing stands in the way",
                "vague": "the change is not concrete",
                "scope": "the change is not scoped to this repository",
                "done": "there is no clear way to tell it is done",
                "decision": "it needs a product, pricing, legal, security or customer-data decision",
                "question": "a worker would have to ask someone something first",
            }}),
        );
    }
    if ask_band {
        questions.insert(
            "band".into(),
            json!({"type": "choice", "instructions": BAND_QUESTION, "criteria": {
                "judgment": "needs judgment",
                "mechanical": "a rote, fully determined edit",
            }}),
        );
    }
    json!({"model": "", "state": state_of(t), "questions": questions})
}

/// What the task looks like to a judge: its title, then its text, cut at
/// `STATE_CHARS`.
fn state_of(t: &Task) -> String {
    let text: String = t.task.chars().take(STATE_CHARS).collect();
    match &t.title {
        Some(title) => format!("{title}\n\n{text}"),
        None => text,
    }
}

/// The probability Jev gave `label` in `answer`, 0 when it named another
/// label without a probability for this one.
fn p_of(answer: &agent::Answer, label: &str) -> f64 {
    answer
        .probabilities
        .get(label)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| {
            if answer.label == label {
                answer.confidence
            } else {
                0.0
            }
        })
}

fn record(f: &Forge, t: &Task, kind: &str, question: &str, answer: &str, why: &str) -> Result<()> {
    let id = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question,
        answer,
        answered_by: "jev",
        citations: why,
        answered_for: None,
    })?;
    f.store.set_decision_kind(id, kind)
}

/// Leave the claimed task blocked on `question`: an attempt row of step
/// `admission` in state needs-input carrying it, which is what `forge
/// requests` shows and `forge answer` answers.
fn block(f: &Forge, t: &Task, question: &str, tried: &str) -> Result<()> {
    let now = crate::unix_now();
    let attempts = f.store.attempts(t.id)?;
    let mut a = Attempt {
        task_id: t.id,
        attempt_no: attempts.len() as i64 + 1,
        step: STEP.into(),
        inputs_json: serde_json::to_string(&f.execution_inputs(std::path::Path::new(&t.worktree)))?,
        step_seq: attempts.iter().map(|a| a.step_seq).max().unwrap_or(0) + 1,
        state: AttemptState::Running,
        started_at: now,
        ..Default::default()
    };
    a.id = f.store.insert_attempt(&a)?;
    let envelope = json!({
        "schema_version": 1,
        "summary": "not admitted for an automated worker",
        "needs_input": {"question": question, "tried": tried, "kind": "question"},
        "checks_run": [],
        "claims": [],
    });
    f.store.finish_attempt(&FinishAttempt {
        id: a.id,
        state: AttemptState::NeedsInput,
        reason: "needs input: admission".into(),
        finished_at: Some(now),
        envelope_json: envelope.to_string(),
        ..Default::default()
    })?;
    let mut t = f.store.task(t.id)?.unwrap_or_else(|| t.clone());
    t.state = TaskState::Blocked;
    t.reason = format!("needs input: {question}");
    t.finished_at = Some(now);
    t.worker_pid = None;
    f.store.update_task(&t)?;
    f.report.emit(t.id, Event::Note { text: &t.reason });
    let _ = crate::audience::emit_ended(f, &t);
    Ok(())
}

/// Record Jev's admission answer: `true` when the task was blocked
/// (below the threshold, or the day's cap spent), `false` when admitted.
fn admit_or_block(f: &Forge, t: &Task, answers: &serde_json::Value) -> Result<bool> {
    let Some(a) = agent::read_answer(&answers["admit"]) else {
        return Ok(false);
    };
    let p_yes = p_of(&a, "yes");
    let gap = agent::read_answer(&answers["gap"])
        .map(|g| g.label)
        .filter(|g| g != "none")
        .unwrap_or_else(|| "unspecified".into());
    let probs = a.probabilities.to_string();
    let today = admitted_today(f)?;
    let over = today >= f.admission.per_day as usize;
    if p_yes >= f.admission.threshold && !over {
        record(
            f,
            t,
            ADMIT,
            QUESTION,
            &format!("admit (p {p_yes:.2})"),
            &probs,
        )?;
    } else {
        let (answer, tried) = if over && p_yes >= f.admission.threshold {
            (
                "block (daily cap)".to_string(),
                format!(
                    "admitted at p {p_yes:.2} but the day's cap of {} admissions is spent",
                    f.admission.per_day
                ),
            )
        } else {
            (
                format!("block (p {p_yes:.2})"),
                format!(
                    "p(admit) {p_yes:.2} under the threshold {:.2}",
                    f.admission.threshold
                ),
            )
        };
        let q = if over && p_yes >= f.admission.threshold {
            format!(
                "Task {} was not admitted for an automated worker: the day's cap of {} admissions is spent (Jev put p {p_yes:.2} on it). Answer to admit it anyway, or withdraw it.",
                t.id, f.admission.per_day
            )
        } else {
            format!(
                "Task {} was not admitted for an automated worker: Jev put p {p_yes:.2} on admitting it (threshold {:.2}), gap: {gap}. Answer to admit it anyway with what it was missing, or withdraw it.",
                t.id, f.admission.threshold
            )
        };
        record(
            f,
            t,
            BLOCK,
            QUESTION,
            &answer,
            &format!("{tried}; gap {gap}; {probs}"),
        )?;
        block(f, t, &q, &tried)?;
        return Ok(true);
    }
    Ok(false)
}

/// The step, on a task this worker has just claimed. `Some` is the task
/// to run (with its band recorded when Jev gave one, and its size
/// predicted and routed on, `size::at_claim`); `None` is a task left
/// blocked, for which nothing is to be started.
pub async fn at_claim(f: &Forge, t: Task) -> Result<Option<Task>> {
    let Some(t) = admit(f, t).await? else {
        return Ok(None);
    };
    Ok(Some(size::at_claim(f, t).await?))
}

/// Admission and the band: `None` is a task left blocked.
async fn admit(f: &Forge, t: Task) -> Result<Option<Task>> {
    let Some(p) = provider(f) else {
        return Ok(Some(t));
    };
    let ask_admit = gated(f, &t)?;
    if ask_admit && let Some(re) = f.admission.exclude.iter().find(|r| r.is_match(&t.task)) {
        let why = format!("the task text matches the exclude pattern {}", re.as_str());
        let q = format!(
            "Task {} was not shown to an automated worker: {why}. Answer to admit it anyway, or withdraw it.",
            t.id
        );
        record(
            f,
            &t,
            EXCLUDE,
            QUESTION,
            "block (excluded, Jev not asked)",
            &why,
        )?;
        block(f, &t, &q, &why)?;
        return Ok(None);
    }
    let ask_band = t.band.is_empty();
    if !ask_admit && !ask_band {
        return Ok(Some(t));
    }
    let asked = agent::ask(p, &request(&t, ask_admit, ask_band), TIMEOUT).await;
    let answers = asked.and_then(|(a, _)| {
        if ask_admit && agent::read_answer(&a["admit"]).is_none() {
            anyhow::bail!("jev returned no answer to the admission question");
        }
        Ok(a)
    });
    let answers = match answers {
        Ok(a) => a,
        Err(e) if ask_admit => {
            let why = format!("Jev could not be asked: {e:#}");
            let q = format!(
                "Task {} was not admitted for an automated worker: {why}. Answer to admit it, or withdraw it.",
                t.id
            );
            record(f, &t, PARK, QUESTION, "park (Jev error)", &why)?;
            block(f, &t, &q, &why)?;
            return Ok(None);
        }
        Err(e) => {
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!("admission: the band was not recorded, Jev errored: {e:#}"),
                },
            );
            return Ok(Some(t));
        }
    };
    if ask_band && let Some(b) = agent::read_answer(&answers["band"]) {
        let p_band = p_of(&b, &b.label);
        f.store.set_band(t.id, &b.label, p_band)?;
        record(
            f,
            &t,
            BAND,
            BAND_QUESTION,
            &format!("{} (p {p_band:.2})", b.label),
            &b.probabilities.to_string(),
        )?;
    }
    if ask_admit && admit_or_block(f, &t, &answers)? {
        return Ok(None);
    }
    f.store.task(t.id)
}

pub mod size;
#[cfg(test)]
pub(crate) mod tests;
