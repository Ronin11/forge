use super::*;

#[test]
fn a_migrations_conflict_tells_the_coder_to_append_after_the_base() {
    let files = vec!["src/store/migrations.rs".to_string()];
    let f = conflict_feedback("main", "forge/base-7", &files);
    assert!(f.contains("never interleave"), "{f}");
    assert!(f.contains("append your branch's new steps after"), "{f}");
    assert!(
        f.contains("migrations_lock_matches_every_shipped_step"),
        "{f}"
    );
    let f = conflict_feedback("main", "forge/base-7", &["src/a.rs".to_string()]);
    assert!(!f.contains("interleave"), "{f}");
}

#[test]
fn render_shows_each_step_and_the_ff_only_instructions() {
    let report = IntegrateReport {
        base_branch: "main".into(),
        base_sha: "abc123456789".into(),
        repo: PathBuf::from("/repo"),
        branch: "forge/integration-1".into(),
        dir: PathBuf::from("/scratch"),
        steps: vec![
            IntegrateStep::Merged {
                task_id: 1,
                branch: "forge/task-1".into(),
                sha: "deadbeef00".into(),
            },
            IntegrateStep::Verified { task_id: 1 },
            IntegrateStep::AlreadyContained { task_id: 2 },
            IntegrateStep::Verified { task_id: 2 },
        ],
        outcome: IntegrateOutcome::Ready,
    };
    assert_eq!(
        report.render(),
        "base     main @ abc12345\n\
         task 1    merged forge/task-1 as deadbeef\n\
         task 1    verified with everything before it\n\
         task 2    already contained\n\
         task 2    verified with everything before it\n\
         integrated 2 task(s) as forge/integration-1 in /repo\n  \
         git -C /repo merge --ff-only forge/integration-1"
    );
}

#[test]
fn render_names_a_conflict_left_out_and_integrates_the_rest() {
    let report = IntegrateReport {
        base_branch: "main".into(),
        base_sha: "abc123456789".into(),
        repo: PathBuf::from("/repo"),
        branch: "forge/integration-1".into(),
        dir: PathBuf::from("/scratch"),
        steps: vec![
            IntegrateStep::Merged {
                task_id: 1,
                branch: "forge/task-1".into(),
                sha: "deadbeef00".into(),
            },
            IntegrateStep::Verified { task_id: 1 },
            IntegrateStep::Conflict {
                task_id: 3,
                files: vec!["answer.txt".into(), "b.txt".into()],
            },
        ],
        outcome: IntegrateOutcome::Ready,
    };
    assert_eq!(
        report.render(),
        "base     main @ abc12345\n\
         task 1    merged forge/task-1 as deadbeef\n\
         task 1    verified with everything before it\n\
         task 3    CONFLICT in answer.txt, b.txt; blocked, left out\n\
         integrated 1 task(s) as forge/integration-1 in /repo\n  \
         git -C /repo merge --ff-only forge/integration-1"
    );
    assert_eq!(
        report.conflicts(),
        vec![(3, &["answer.txt".to_string(), "b.txt".to_string()][..])]
    );
}

#[test]
fn render_stops_when_the_merged_tree_fails_verification() {
    let report = IntegrateReport {
        base_branch: "main".into(),
        base_sha: "abc123456789".into(),
        repo: PathBuf::from("/repo"),
        branch: "forge/integration-1".into(),
        dir: PathBuf::from("/scratch"),
        steps: vec![IntegrateStep::Merged {
            task_id: 2,
            branch: "forge/task-2".into(),
            sha: "cafef00d00".into(),
        }],
        outcome: IntegrateOutcome::VerifyFailed {
            task_id: 2,
            reason: "shell check failed".into(),
            completed: 1,
        },
    };
    assert_eq!(
        report.render(),
        "base     main @ abc12345\n\
         task 2    merged forge/task-2 as cafef00d\n\
         task 2    checks FAIL after merging: shell check failed\n\
         stopped after 1 task(s); the scratch clone is at /scratch"
    );
}
