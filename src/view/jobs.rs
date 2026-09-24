use crate::ctx::Forge;
use anyhow::Result;
use serde::Serialize;

/// The document `forge job show ID --json` prints: one job with every
/// step and effect it recorded, newest-run fields alongside them.
#[derive(Serialize)]
pub struct JobDoc {
    pub id: i64,
    pub project: String,
    pub workflow: String,
    pub workflow_hash: String,
    pub landed_sha: String,
    pub trigger_kind: String,
    pub trigger_ref: String,
    pub state: String,
    /// `"repo"` or `"catalog"` (see `store::Job::workflow_source`).
    pub workflow_source: String,
    pub dry_run: bool,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub cost_usd: Option<f64>,
    pub verdict_json: String,
    /// When this job becomes claimable, a unix second; `None` for a job
    /// that was never delayed (see `store::Job::due_at`).
    pub due_at: Option<i64>,
    /// How many times `[limits] on_failure = "retry:N"` has already
    /// requeued this job's lineage (see `store::Job::retry_count`).
    pub retry_count: i64,
    pub steps: Vec<crate::store::JobStep>,
    pub effects: Vec<crate::store::JobEffect>,
}

pub fn job_doc(f: &Forge, j: &crate::store::Job) -> Result<JobDoc> {
    let steps = f.store.job_steps(j.id)?;
    let effects = f.store.job_effects(j.id)?;
    Ok(JobDoc {
        id: j.id,
        project: j.project.clone(),
        workflow: j.workflow.clone(),
        workflow_hash: j.workflow_hash.clone(),
        landed_sha: j.landed_sha.clone(),
        trigger_kind: j.trigger_kind.clone(),
        trigger_ref: j.trigger_ref.clone(),
        state: j.state.as_str().to_string(),
        workflow_source: j.workflow_source.clone(),
        dry_run: j.dry_run,
        started_at: j.started_at,
        finished_at: j.finished_at,
        cost_usd: j.cost_usd,
        verdict_json: j.verdict_json.clone(),
        due_at: j.due_at,
        retry_count: j.retry_count,
        steps,
        effects,
    })
}
