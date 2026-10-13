//! The size prediction at the claim (docs/EXECUTION.md, "Admission and
//! the judgment band", 3). A local model trained on the repository's own
//! landings (`[admission] size_provider`; docs/research/indecis-size-
//! 2026-10-13.md) is asked how large the change will be, in lines changed
//! at landing: `small`, `medium` or `large`. The answer is recorded on the
//! task (`predicted_size`, `predicted_size_p`) and as a `size` decision
//! row, and `[admission] size_workflows` routes the workflow on it: a task
//! whose request named no workflow itself moves to the workflow mapped
//! for its size when the probability reaches `size_threshold`, its
//! `workflow_source` becoming `size`. A flag-named workflow, a size with
//! no mapping, a workflow that does not fit the repository or the task's
//! trust, or a model error leaves the task as it was; the decision row
//! says which.

use super::*;
use crate::store::Reroute;

/// The decision kind of a prediction, routed or not.
pub const KIND: &str = "size";
/// The sizes, in order, as the model was trained on them.
pub const LEVELS: [&str; 3] = ["small", "medium", "large"];
/// A change of at most this many lines is `small`, ...
pub const SMALL_LINES: u64 = 50;
/// ... at most this many `medium`, past it `large`.
pub const MEDIUM_LINES: u64 = 300;
const TIMEOUT: Duration = Duration::from_secs(15);

/// The question, word for word what the model was trained on
/// (`forge eval jev`'s size set).
pub fn instructions() -> String {
    format!(
        "How large is the change this task asks for, in lines changed at landing (small is at most {SMALL_LINES}, medium at most {MEDIUM_LINES})?"
    )
}

/// The size a change of `lines` lines is labeled with.
pub fn label(lines: u64) -> &'static str {
    match lines {
        _ if lines <= SMALL_LINES => "small",
        _ if lines <= MEDIUM_LINES => "medium",
        _ => "large",
    }
}

/// The one question, under the name the trained head answers.
fn request(t: &Task) -> Value {
    json!({"model": "", "state": state_of(t), "questions": {
        crate::workflows::OUTCOME_QUESTION: {
            "type": "score", "instructions": instructions(), "criteria": LEVELS,
        },
    }})
}

/// The provider asked, when there is one: `[admission] size_provider`
/// naming a jev provider, with the step on.
fn provider(f: &Forge) -> Option<&Provider> {
    if !f.admission.enabled {
        return None;
    }
    f.providers
        .get(f.admission.size_provider.as_deref()?)
        .filter(|p| p.runner == Runner::Jev)
}

/// Why the task keeps its workflow, or the move made.
async fn route(f: &Forge, t: &mut Task, size: &str, p: f64) -> Result<String> {
    let kept = |why: String| format!("kept {}: {why}", t.workflow);
    let Some(name) = f.admission.size_workflows.get(size) else {
        return Ok(kept(format!("no workflow is mapped for {size}")));
    };
    if t.workflow_source == "flag" {
        return Ok(kept("the request named it".into()));
    }
    if p < f.admission.size_threshold {
        return Ok(kept(format!(
            "p {p:.2} is under the routing threshold {:.2}",
            f.admission.size_threshold
        )));
    }
    if *name == t.workflow {
        return Ok(kept(format!("already the workflow for {size}")));
    }
    let (wf, tdd) = match crate::queue::fitted(f, t, name).await {
        Ok(fit) => fit,
        Err(e) => return Ok(kept(format!("{name} does not fit: {e:#}"))),
    };
    f.store.reroute_workflow(
        t.id,
        &Reroute {
            workflow: name,
            hash: &wf.hash,
            text: &wf.text,
            tdd,
            source: "size",
        },
    )?;
    let was = std::mem::replace(&mut t.workflow, name.clone());
    t.workflow_hash = wf.hash;
    t.workflow_text = wf.text;
    t.shape_tdd = tdd;
    t.workflow_source = "size".into();
    t.actions_json.clear();
    Ok(format!("routed {was} -> {name}"))
}

/// The step, after admission, on a task this worker runs: predict once,
/// record, route. A model error is a note, never a block.
pub(super) async fn at_claim(f: &Forge, mut t: Task) -> Result<Task> {
    let Some(p) = provider(f) else {
        return Ok(t);
    };
    if !t.predicted_size.is_empty() {
        return Ok(t);
    }
    let answer = match agent::ask(p, &request(&t), TIMEOUT).await {
        Ok((a, _)) => agent::read_answer(&a[crate::workflows::OUTCOME_QUESTION]),
        Err(e) => {
            let text = format!("size     not predicted, {} errored: {e:#}", p.name);
            f.report.emit(t.id, Event::Note { text: &text });
            return Ok(t);
        }
    };
    let Some(a) = answer else {
        let text = format!("size     not predicted, {} gave no answer", p.name);
        f.report.emit(t.id, Event::Note { text: &text });
        return Ok(t);
    };
    let p_size = p_of(&a, &a.label);
    f.store.set_predicted_size(t.id, &a.label, p_size)?;
    t.predicted_size = a.label.clone();
    t.predicted_size_p = Some(p_size);
    let routed = route(f, &mut t, &a.label, p_size).await?;
    let id = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &instructions(),
        answer: &format!("{} (p {p_size:.2})", a.label),
        answered_by: &p.name,
        citations: &format!("{}; {routed}", a.probabilities),
        answered_for: None,
    })?;
    f.store.set_decision_kind(id, KIND)?;
    let text = format!("size     {} (p {p_size:.2}); {routed}", a.label);
    f.report.emit(t.id, Event::Note { text: &text });
    Ok(t)
}
