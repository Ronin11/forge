//! Input publication: scheduled rows without a due date cannot be claimed.
use super::*;

pub(super) fn publish(f: &Forge, id: i64, text: &str, due_at: Option<i64>) -> Result<()> {
    let dir = input_dir(f, id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("input.json"), text)?;
    anyhow::ensure!(
        f.store
            .publish_job(id, scheduled_state(due_at, unix_now()), due_at)?,
        "job {id} is no longer awaiting input"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::{Project, Store};

    fn setup() -> (tempfile::TempDir, Arc<Forge>, i64) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        store
            .create_project(&Project {
                name: "input".into(),
                purpose: "test".into(),
                ..Default::default()
            })
            .unwrap();
        let id = store
            .create_job(&Job {
                project: "input".into(),
                state: JobState::Scheduled,
                ..Default::default()
            })
            .unwrap();
        let f = Forge::open_with(
            Paths {
                home: dir.path().into(),
                worktrees: dir.path().join("worktrees"),
                logs: dir.path().join("logs"),
            },
            store,
        )
        .unwrap();
        (dir, Arc::new(f), id)
    }

    #[test]
    fn input_publication_blocks_claims_until_saved_and_preserves_delays() {
        for due_at in [None, Some(1), Some(unix_now() + 3600)] {
            let (_dir, f, id) = setup();
            let worker = Store::open(&f.paths.home.join("forge.db")).unwrap();
            assert!(worker.claim_next_job().unwrap().is_none());
            publish(&f, id, r#"{"contact":"alice","text":"quote"}"#, due_at).unwrap();
            let job = worker.job(id).unwrap().unwrap();
            assert_eq!(job.due_at, due_at);
            let claimed = worker.claim_next_job().unwrap();
            assert_eq!(claimed.is_some(), due_at.is_none_or(|d| d <= unix_now()));
            assert_eq!(
                std::fs::read_to_string(input_dir(&f, id).join("input.json")).unwrap(),
                r#"{"contact":"alice","text":"quote"}"#
            );
        }
    }

    #[test]
    fn failed_input_write_leaves_job_unclaimable() {
        let (_dir, f, id) = setup();
        std::fs::write(&f.paths.worktrees, "not a directory").unwrap();
        assert!(publish(&f, id, "{}", None).is_err());
        assert!(f.store.claim_next_job().unwrap().is_none());
    }

    #[test]
    fn publication_does_not_revive_a_withdrawn_job() {
        let (_dir, f, id) = setup();
        assert!(f.store.withdraw_job(id).unwrap());
        assert!(publish(&f, id, "{}", None).is_err());
        assert!(f.store.claim_next_job().unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_input_fails_claimed_job_without_running_steps() {
        let (_dir, f, id) = setup();
        assert!(f.store.publish_job(id, JobState::Queued, None).unwrap());
        assert_eq!(f.store.claim_next_job().unwrap().unwrap().id, id);
        assert_eq!(drive(f.clone(), id).await, JobState::Failed);
        let job = f.store.job(id).unwrap().unwrap();
        assert!(
            job.verdict_json.contains("input.json"),
            "{}",
            job.verdict_json
        );
        assert!(f.store.job_steps(id).unwrap().is_empty());
        assert!(!input_dir(&f, id).join("input.json").exists());
    }
}
