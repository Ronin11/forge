use super::*;
use crate::ctx::Paths;
use crate::store::{InsertDecisionBy, Store};

#[test]
fn base_choice_preserves_unlanded_work_and_verified_retry_recovery() {
    assert!(reusable_branch(false, true));
    assert!(reusable_branch(true, true));
    // Already landed verified branches still need setup recovery on retry.
    assert!(reusable_branch(true, false));
    assert!(!reusable_branch(false, false));
}

async fn checkout_case(conflict: bool, landed: bool, refile: bool) {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path().to_path_buf()).unwrap();
    let store = Store::open(&home.path().join("forge.db")).unwrap();
    let f = Forge::open_with(paths, store).unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("shared.txt"), "original\n").unwrap();
    let base = git::init_commit_all(repo.path(), "base").await.unwrap();
    std::fs::write(repo.path().join("shared.txt"), "predecessor\n").unwrap();
    std::fs::write(repo.path().join("prior.txt"), "implementation\n").unwrap();
    let tip = git::commit_all(repo.path(), "prior implementation")
        .await
        .unwrap()
        .unwrap();
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["branch", "prior", &tip])
            .status()
            .unwrap()
            .success()
    );
    if !landed {
        git::reset_hard(repo.path(), &base).await.unwrap();
    }
    let path = if conflict { "shared.txt" } else { "base.txt" };
    std::fs::write(repo.path().join(path), "current base\n").unwrap();
    let current = git::commit_all(repo.path(), "base moved")
        .await
        .unwrap()
        .unwrap();
    let mut parent = Task {
        repo: repo.path().display().to_string(),
        worktree: repo.path().display().to_string(),
        branch: "prior".into(),
        base_sha: base,
        task: "original".into(),
        state: TaskState::Failed,
        ..Default::default()
    };
    parent.id = f.store.insert_task(&parent).unwrap();
    f.store.update_task(&parent).unwrap();
    let mut child = Task {
        repo: parent.repo.clone(),
        base_sha: current.clone(),
        task: "follow up".into(),
        retry_of: (!refile).then_some(parent.id),
        ..Default::default()
    };
    child.id = f.store.insert_task(&child).unwrap();
    f.store.update_task(&child).unwrap();
    if refile {
        let d = f
            .store
            .insert_decision_by(InsertDecisionBy {
                task_id: parent.id,
                repo: &parent.repo,
                question: "failed",
                answer: "refile",
                answered_by: "mechanic",
                citations: "ratchet",
                answered_for: None,
            })
            .unwrap();
        f.store.set_decision_kind(d, "mechanic-ratchet").unwrap();
        f.store.set_decision_retry(d, child.id).unwrap();
    }
    let dir = home.path().join("checkout");
    let branch = git::current_branch(repo.path()).await.unwrap();
    git::clone_task(repo.path(), &branch, &dir, "followup", None, None)
        .await
        .unwrap();
    let old = branch_parent(&f, &child).ok().flatten().unwrap();
    let merged = reuse_branch(&f, &mut child, old, &dir).await.ok().unwrap();
    assert_eq!(merged, !conflict && !landed);
    if conflict || landed {
        assert_eq!(git::head(&dir).await.unwrap(), current);
    } else {
        assert!(git::is_ancestor(&dir, &tip, "HEAD").await);
        assert!(git::is_ancestor(&dir, &current, "HEAD").await);
        assert_eq!(
            std::fs::read_to_string(dir.join("prior.txt")).unwrap(),
            "implementation\n"
        );
    }
    if !landed {
        assert!(
            child.task.contains("prior implementation"),
            "{}",
            child.task
        );
        assert!(child.task.contains("prior.txt"), "{}", child.task);
    }
    if conflict {
        assert!(
            child.task.contains("branch prior conflicts in shared.txt"),
            "{}",
            child.task
        );
        assert!(child.task.contains("Cherry-pick"));
        assert!(!dir.join("prior.txt").exists());
    }
}

#[tokio::test]
async fn unverified_retry_merges_current_base_and_keeps_prior_code() {
    checkout_case(false, false, false).await;
}

#[tokio::test]
async fn unverified_conflict_starts_at_base_and_carries_recovery_context() {
    checkout_case(true, false, false).await;
}

#[tokio::test]
async fn landed_unverified_branch_starts_at_base() {
    checkout_case(false, true, false).await;
}

#[tokio::test]
async fn automatic_refile_inherits_code_without_joining_retry_lineage() {
    checkout_case(false, false, true).await;
}
