//! Answers to job and deploy questions: blocked no-work tasks with no
//! attempt to retry. Recording an answer closes the question itself.

use super::*;

/// Whether `t` is a blocked job or deploy question with no attempt to retry.
pub(super) fn is_no_work_question(f: &Forge, t: &Task) -> Result<bool> {
    Ok(t.state == TaskState::Blocked
        && (t.task == "job question" || (t.task == "deploy question" && t.deploy_id.is_some()))
        && t.workflow == "direct"
        && f.store.attempts(t.id)?.is_empty())
}

/// Record the answer and settle the question without queuing anything.
/// A deploy question is withdrawn; a job question succeeds and closes
/// the job's `needs_human` resolution. Returns the decision and task.
pub(super) fn answer(
    f: &Forge,
    old: &Task,
    text: &str,
    by: &str,
    citations: &str,
) -> Result<(i64, Task)> {
    let job_id = if old.deploy_id.is_none() {
        crate::job::question_job_id(&old.reason)
    } else {
        None
    };
    let recorded = match (old.deploy_id, job_id) {
        (Some(deploy), _) => format!("deploy {deploy} answered: {text}"),
        (_, Some(job)) => format!("job {job} answered: {text}"),
        _ => format!("job question answered: {text}"),
    };
    let decision = f.store.answer_blocked_question(
        crate::store::InsertDecisionBy {
            task_id: old.id,
            repo: &old.repo,
            question: &old.reason,
            answer: &recorded,
            answered_by: by,
            citations,
            answered_for: old.question_to.as_deref(),
        },
        text,
    )?;
    if let Some(job_id) = job_id.filter(|&j| f.store.job(j).ok().flatten().is_some()) {
        f.store.resolve_job(&crate::store::JobResolution {
            job_id,
            resolution: "answered".to_string(),
            answer: text.to_string(),
            answered_by: by.to_string(),
            task_id: Some(old.id),
            at: crate::unix_now(),
        })?;
    }
    let settled = f
        .store
        .task(old.id)?
        .with_context(|| format!("no task {}", old.id))?;
    Ok((decision, settled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Attempt, AttemptState, FinishAttempt, Job, JobState, Project, Store};

    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = crate::ctx::Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        f.store
            .create_project(&Project {
                name: "demo".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        (dir, f)
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let o = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}");
    }

    fn insert(f: &Forge, mut t: Task) -> Task {
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        t
    }

    fn blocked(repo: &str, task: &str, reason: &str, question_to: Option<&str>) -> Task {
        Task {
            repo: repo.into(),
            task: task.into(),
            base_branch: "main".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Blocked,
            reason: reason.into(),
            question_to: question_to.map(str::to_string),
            created_at: 1,
            workflow: "direct".into(),
            project: Some("demo".into()),
            land: false,
            ..Default::default()
        }
    }

    fn job_asking(f: &Forge) -> i64 {
        f.store
            .create_job(&Job {
                project: "demo".into(),
                workflow: "drift-weekly".into(),
                trigger_kind: "schedule".into(),
                state: JobState::NeedsHuman,
                workflow_source: "repo".into(),
                started_at: 1,
                verdict_json: "[]".into(),
                ..Default::default()
            })
            .unwrap()
    }

    #[tokio::test]
    async fn answering_a_job_question_settles_it_as_the_jobs_resolution() {
        let (_dir, f) = fixture();
        let job = job_asking(&f);
        let reason = format!("job {job} (drift-weekly) failed: drift: 3 files\n\nEffects:\nnone");
        let t = insert(&f, blocked("/repo", "job question", &reason, None));

        let (decision, settled) = answer(&f, &t, "ignore this week", "operator", "").unwrap();
        assert_eq!(settled.id, t.id);
        assert_eq!(settled.state, TaskState::Succeeded);
        assert_eq!(settled.reason, "ignore this week");
        assert_eq!(f.store.queued_count().unwrap(), 0, "nothing is queued");
        let d = f
            .store
            .decisions_in_lineage(t.id)
            .unwrap()
            .into_iter()
            .find(|d| d.id == decision)
            .unwrap();
        assert_eq!(d.answer, format!("job {job} answered: ignore this week"));
        let r = f.store.job_resolution(job).unwrap().unwrap();
        assert_eq!(r.resolution, "answered");
        assert_eq!(r.answer, "ignore this week");
        assert_eq!(r.task_id, Some(t.id));
        let doc = crate::view::job_doc(&f, &f.store.job(job).unwrap().unwrap()).unwrap();
        assert_eq!(doc.resolution.unwrap().answer, "ignore this week");
    }

    #[tokio::test]
    async fn queue_answer_takes_a_job_question_from_the_contact_it_was_addressed_to() {
        let (_dir, f) = fixture();
        let job = job_asking(&f);
        let reason = format!("job {job} (quote) failed: send: exit 1, no output\n\nEffects:\nnone");
        let t = insert(&f, blocked("/repo", "job question", &reason, Some("alice")));

        let refused = super::super::answer(&f, t.id, "yes", "alice", "", Some(("demo", "bob")))
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("addressed to alice"), "{refused}");
        let (_, settled) =
            super::super::answer(&f, t.id, "resend it", "alice", "", Some(("demo", "alice")))
                .await
                .unwrap();
        assert_eq!(settled.id, t.id);
        assert_eq!(settled.state, TaskState::Succeeded);
        let r = f.store.job_resolution(job).unwrap().unwrap();
        assert_eq!(r.answered_by, "alice");
        let again = super::super::answer(&f, t.id, "again", "alice", "", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(again.contains("not blocked on a question"), "{again}");
    }

    #[tokio::test]
    async fn a_deploy_question_checks_scope_and_records_its_own_recipient() {
        let (_dir, f) = fixture();
        let deploy = f
            .store
            .start_deploy("demo", "prod", "sha", 1, None)
            .unwrap();
        let mut t = blocked("/repo", "deploy question", "deploy failed", Some("alice"));
        t.deploy_id = Some(deploy);
        let t = insert(&f, t);
        let refused = super::super::answer(&f, t.id, "ok", "bob", "", Some(("demo", "bob")))
            .await
            .unwrap_err()
            .to_string();
        assert!(refused.contains("addressed to alice"), "{refused}");
        assert_eq!(
            f.store.task(t.id).unwrap().unwrap().state,
            TaskState::Blocked
        );
        let (_, settled) = super::super::answer(
            &f,
            t.id,
            "ok",
            "alice",
            "deploy log",
            Some(("demo", "alice")),
        )
        .await
        .unwrap();
        assert_eq!(settled.state, TaskState::Withdrawn);
        assert_eq!(settled.deploy_id, Some(deploy));
        assert!(settled.finished_at.is_some());
        assert_eq!(f.store.queued_count().unwrap(), 0);
        assert!(f.store.attempts(t.id).unwrap().is_empty());
        let decisions = f.store.decisions_in_lineage(t.id).unwrap();
        assert_eq!(decisions.len(), 1);
        let d = &decisions[0];
        assert_eq!(d.question, "deploy failed");
        assert_eq!(d.answer, format!("deploy {deploy} answered: ok"));
        assert_eq!(d.answered_by, "alice");
        assert_eq!(d.answered_for.as_deref(), Some("alice"));
        assert_eq!(d.citations, "deploy log");
        assert_eq!(d.retry_id, Some(t.id));
        assert!(answer(&f, &t, "again", "operator", "").is_err());
        assert_eq!(f.store.decisions_in_lineage(t.id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_task_blocked_on_an_attempts_question_still_requeues() {
        let (dir, f) = fixture();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["config", "user.name", "Test"]);
        git_in(&repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.join("forge.toml"), "[checks]\nok = [\"true\"]\n").unwrap();
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-qm", "init"]);
        f.store
            .register_repo("demo", repo.to_str().unwrap(), None)
            .unwrap();
        let repo_str = repo.canonicalize().unwrap().display().to_string();
        let t = insert(
            &f,
            blocked(&repo_str, "rename the widget", "needs input: which?", None),
        );
        let envelope = serde_json::json!({
            "schema_version": 1,
            "summary": "",
            "needs_input": { "question": "which widget?", "tried": "looked" }
        })
        .to_string();
        let attempt = f
            .store
            .insert_attempt(&Attempt {
                task_id: t.id,
                attempt_no: 1,
                step: "code".into(),
                state: AttemptState::NeedsInput,
                started_at: 1,
                ..Default::default()
            })
            .unwrap();
        f.store
            .finish_attempt(&FinishAttempt {
                id: attempt,
                state: AttemptState::NeedsInput,
                envelope_json: envelope,
                ..Default::default()
            })
            .unwrap();
        assert!(!is_no_work_question(&f, &t).unwrap());

        let (decision, n) = super::super::answer(&f, t.id, "the blue one", "operator", "", None)
            .await
            .unwrap();
        assert_ne!(n.id, t.id, "a retry, not the task itself");
        assert_eq!(n.retry_of, Some(t.id));
        assert!(n.task.contains("the blue one"), "{}", n.task);
        let d = f
            .store
            .decisions_in_lineage(t.id)
            .unwrap()
            .into_iter()
            .find(|d| d.id == decision)
            .unwrap();
        assert_eq!(d.retry_id, Some(n.id));
        assert_eq!(d.question, "which widget?");
    }
}
