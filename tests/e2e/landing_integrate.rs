//! `forge integrate` pre-checks every branch with `git merge-tree`: the
//! branches that merge cleanly integrate, and one that would conflict with
//! what merged before it is left out and its task blocked naming the files.

use crate::support::*;

#[test]
fn integrate_lands_the_clean_branches_and_blocks_the_one_that_would_conflict_naming_the_file() {
    let e = Env::new();
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "shell only"]);
    // Tasks 1 and 2 write the same line of answer.txt; task 3 touches
    // nothing either of them does. None of them lands on its own.
    for (fake, task) in [
        ("echoanswer.sh", "write 42 to answer.txt"),
        ("echoanswer.sh", "write 43 to answer.txt"),
        ("addfile.sh", "add extra"),
    ] {
        let o = e.forge(
            fake,
            &[
                "run",
                e.repo.to_str().unwrap(),
                task,
                "--no-land",
                "--retries",
                "0",
            ],
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    for id in [1, 2, 3] {
        assert_eq!(e.task(id).0, "succeeded", "task {id}: {}", e.task(id).1);
    }

    let o = e.forge("ok.sh", &["integrate", "1", "2", "3"]);
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    // The conflict is reported, but it does not stop the run.
    assert!(!o.status.success(), "{out}{err}");
    assert!(err.contains("task 2 would conflict in answer.txt"), "{err}");
    assert!(
        out.contains("task 2    CONFLICT in answer.txt; blocked, left out"),
        "{out}"
    );
    assert!(
        out.contains("task 1    verified with everything before it")
            && out.contains("task 3    verified with everything before it"),
        "{out}"
    );
    let branch = out
        .lines()
        .find(|l| l.starts_with("integrated 2 task(s)"))
        .unwrap_or_else(|| panic!("{out}"))
        .split_whitespace()
        .find(|w| w.starts_with("forge/integration-"))
        .unwrap()
        .to_string();
    assert_eq!(
        git(&e.repo, &["show", &format!("{branch}:answer.txt")]),
        "42"
    );
    assert_eq!(
        git(&e.repo, &["show", &format!("{branch}:extra.txt")]),
        "extra"
    );

    let (state, reason, _) = e.task(2);
    assert_eq!(state, "blocked", "{reason}");
    assert!(
        reason.starts_with("would conflict in answer.txt"),
        "{reason}"
    );
    assert!(reason.contains("task(s) 1"), "{reason}");
    for id in [1, 3] {
        assert_eq!(e.task(id).0, "succeeded", "task {id}");
    }
}
