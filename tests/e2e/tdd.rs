use crate::support::*;

#[test]
fn tdd_hides_the_tests_and_verifies_the_coder_against_them() {
    let e = Env::new();
    tdd_repo(&e);
    assert!(
        run_tdd(&e, "ok.sh", "testwriter.sh", "make answer.txt contain 42")
            .status
            .success()
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    let c = e.db();
    let steps: Vec<String> = c
        .prepare("SELECT step FROM attempts WHERE task_id=1 ORDER BY attempt_no")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(steps, vec!["tests", "code"]);
    assert_eq!(check(&a[0].4, "L0", "namespace-only"), Some(true));
    assert_eq!(check(&a[0].4, "L1", "red-on-base"), Some(true));
    assert_eq!(check(&a[1].4, "L0", "namespace-untouched"), Some(true));
    assert_eq!(
        check(&a[1].4, "L1", "test"),
        Some(true),
        "the hidden test ran against the coder's tree"
    );
    let coder_prompt = e.log_text(1, 2);
    assert!(
        coder_prompt.contains("They expect this interface"),
        "{coder_prompt}"
    );
    assert!(
        coder_prompt.contains("entire content is the line 42"),
        "{coder_prompt}"
    );
    assert!(
        !coder_prompt.contains("grep -qx"),
        "assertions stay hidden:\n{coder_prompt}"
    );
    assert!(
        !e.home
            .join("worktrees/1/tests/acceptance/answer.sh")
            .exists(),
        "the overlay is removed afterwards"
    );
    assert_eq!(
        git(&e.home.join("worktrees/1"), &["remote"]),
        "",
        "the coder's clone has no remote to fetch hidden tests from"
    );
    assert!(
        git(&e.repo, &["branch"]).contains("verify/1"),
        "the tests live on verify/<id> in the repo"
    );
    assert!(
        e.origin_branches().contains("verify/1"),
        "and on the remote for review"
    );
    assert!(e.task(1).2, "pushed");
}

#[test]
fn tdd_rejects_tests_that_pass_on_base_and_coders_that_shadow_the_namespace() {
    let e = Env::new();
    tdd_repo(&e);
    assert!(!run_tdd(&e, "ok.sh", "greentests.sh", "x").status.success());
    let a = e.attempts(1);
    assert_eq!(a.len(), 1, "the code step never runs");
    assert_eq!(a[0].2, "L1 failed: red-on-base");

    assert!(
        !run_tdd(&e, "shadow.sh", "testwriter.sh", "y")
            .status
            .success()
    );
    let a = e.attempts(2);
    assert_eq!(a[1].2, "L0 failed: namespace-untouched");
}

#[test]
fn tdd_is_refused_without_a_namespace_or_a_test_check() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &["add", e.repo.to_str().unwrap(), "x", "--workflow", "tdd"],
    );
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("needs [verify] namespace"));
    let o = e.forge(
        "ok.sh",
        &["add", e.repo.to_str().unwrap(), "x", "--workflow", "nope"],
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown workflow"));
    let o = e.forge("ok.sh", &["workflows"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("tests → setup → repo-map → code"), "{out}");
    assert!(
        out.contains("measured   unknown (0 of 5 runs needed)"),
        "{out}"
    );
    let o = e.forge("ok.sh", &["workflows", "--json"]);
    let docs: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let tdd = docs["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "tdd")
        .unwrap()
        .clone();
    assert_eq!(tdd["name"], "tdd");
    assert_eq!(tdd["measured"]["current"]["known"], false);
    assert_eq!(tdd["measured"]["cost_vs_direct"], serde_json::Value::Null);
}

#[test]
fn a_check_failing_inside_the_hidden_tests_goes_back_to_the_test_author() {
    let e = Env::new();
    tdd_repo(&e);
    let toml = std::fs::read_to_string(e.repo.join("forge.toml")).unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        toml.replace(
            "[checks]\n",
            "[checks]\nlint = [\"bash\", \"-c\", \"! grep -rn TODO tests/acceptance\"]\n",
        ),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "lint rejects TODO"]);
    let mut c = e.with_role("ok.sh", "TESTS", "testwriter-todo.sh");
    let o = c
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42",
            "--workflow",
            "tdd",
            "--retries",
            "1",
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert!(
        err.contains("lint failed inside tests/acceptance/; back to tests for another attempt"),
        "{err}"
    );
    let a = e.attempts(1);
    let states: Vec<&str> = a.iter().map(|x| x.1.as_str()).collect();
    assert_eq!(
        states,
        vec!["succeeded", "checks_failed", "succeeded", "succeeded"],
        "tests, coder (lint on the hidden file), tests again, coder again"
    );
    assert!(e.task(1).2, "pushed");

    // With no attempt left for the test author, the task fails and says whose fault it was.
    let mut c = e.with_role("ok.sh", "TESTS", "testwriter-todo.sh");
    let o = c
        .args([
            "run",
            "--no-land",
            e.repo.to_str().unwrap(),
            "write 42 again",
            "--workflow",
            "tdd",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let (state, reason, _) = e.task(2);
    assert_eq!(state, "failed");
    assert!(
        reason.starts_with(
            "check lint failed inside the verification namespace after 1 tests attempt(s):"
        ),
        "{reason}"
    );
    let o = e.forge("ok.sh", &["show", "2"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("test author could not fix them"));
}
