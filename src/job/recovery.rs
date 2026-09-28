//! Interrupted runs retain their files and cumulative accounting.
use super::*;

pub(super) fn run_dir(f: &Forge, id: i64) -> Result<PathBuf> {
    Ok(input_dir(f, id).join(format!("run-{}", f.store.job_run(id)?)))
}

pub(super) fn prepare_run(f: &Forge, id: i64, text: &str) -> Result<PathBuf> {
    let dir = run_dir(f, id)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(input_dir(f, id).join("input.json"), text)?;
    std::fs::write(dir.join("input.json"), text)?;
    Ok(dir)
}

fn recovery_dir(f: &Forge, id: i64) -> Result<PathBuf> {
    let dir = run_dir(f, id)?;
    // A worker interrupted before this migration wrote directly in input_dir.
    Ok(if dir.exists() { dir } else { input_dir(f, id) })
}

pub(crate) fn recover_interrupted(f: &Forge, job_id: i64, owner: &Owner) -> Result<()> {
    // Another worker claimed the job since it was listed: it is not ours to
    // recover, and mirroring its `effects.log` would record its effects twice.
    if f.store.job_owner(job_id)?.as_ref() != Some(owner) {
        return Ok(());
    }
    let job = f.store.job(job_id)?.context("interrupted job vanished")?;
    let mut effects = f.store.job_effects(job_id)?;
    let logged = log_lines(&recovery_dir(f, job_id)?.join("effects.log"));
    for line in logged.iter().skip(effects.len()) {
        let mut parts = line.splitn(3, '\t');
        let (Some(kind), Some(target), Some(summary)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let effect = JobEffect {
            id: 0,
            job_id,
            seq: -1,
            kind: kind.to_string(),
            target: target.to_string(),
            summary: format!("(recovered) {summary}"),
            dry_run: job.dry_run,
        };
        f.store.append_job_effect(&effect)?;
        effects.push(effect);
    }
    let previous = owner
        .pid
        .map_or_else(|| "unknown".into(), |pid| pid.to_string());
    let mut reason = format!("previous worker {previous} exited");
    if effects.is_empty() {
        // Recorded before the requeue so it belongs to the run it interrupted.
        f.store.append_job_step(&JobStep {
            job_id,
            action: "recovery".into(),
            kind: "operation".into(),
            tail: reason.clone(),
            ..Default::default()
        })?;
        if f.store.requeue_job(job_id, owner)? {
            eprintln!("requeued job {job_id}: {reason}");
        }
        return Ok(());
    }
    for effect in &effects {
        reason.push_str(&format!(
            "\n{} {}: {}",
            effect.kind, effect.target, effect.summary
        ));
    }
    reason.push_str("\nAutomatic retry suppressed: effects already performed.");
    let repo = f.store.first_repo(&job.project)?.unwrap_or_default();
    let policy = workflows::resolve_job_for_project(
        &f.paths.home,
        Path::new(&repo),
        &job.landed_sha,
        &job.workflow,
    );
    // A missing workflow must not hide interrupted external work.
    let action = match policy {
        Ok((wf, _, _)) => {
            let input = std::fs::read_to_string(input_dir(f, job_id).join("input.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null);
            let contact = trigger_contact(&job, wf.trigger.as_ref(), &input);
            wf.limits
                .as_ref()
                .map_or(FailureAction::Ask(None), |limits| {
                    decide_on_failure(&limits.on_failure, job.retry_count, contact.as_deref())
                })
        }
        Err(_) => FailureAction::Ask(None),
    };
    let verdict = executor_error_verdict(&anyhow::anyhow!("{reason}"));
    if !f
        .store
        .finish_orphaned_job(job_id, owner, JobState::Failed, &verdict)?
    {
        return Ok(());
    }
    f.report.emit(
        0,
        Event::JobFinished {
            project: &job.project,
            workflow: &job.workflow,
            job_id,
            state: JobState::Failed.as_str(),
            cost_usd: f.store.job_step_cost(job_id)?,
        },
    );
    match action {
        FailureAction::Stop => {}
        FailureAction::Ask(to) => ask(f, &job.project, &repo, to.as_deref(), reason.clone())?,
        FailureAction::Retry => ask(f, &job.project, &repo, None, reason.clone())?,
    }
    Ok(())
}
