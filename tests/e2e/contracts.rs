use crate::support::*;

#[test]
fn the_review_contract_demotes_only_with_executed_evidence_and_never_writes() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    // 1: a demotion backed by a tool call blocks the task, and the branch is still pushed.
    assert!(
        !run_wf(
            &e,
            "ok.sh",
            &[("FORGE2_CLAUDE_BIN_REVIEW", "reviewer-demote.sh")],
            "reviewed",
            "write 42"
        )
        .status
        .success()
    );
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "blocked");
    assert!(
        reason.starts_with("review demoted: answer.txt is 42"),
        "{reason}"
    );
    assert!(
        pushed,
        "the branch passed the checks; the human needs to see it"
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert_eq!(a[1].0, 2);
    assert_eq!(check(&a[1].4, "L0", "no-writes"), Some(true));
    assert_eq!(check(&a[1].4, "note", "executed-something"), Some(true));
    let o = e.forge("ok.sh", &["show", "1"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Reviewer precision"));
    // 2: a demotion with no tool call is an opinion: ignored, task succeeds.
    assert!(
        run_wf(
            &e,
            "ok.sh",
            &[("FORGE2_CLAUDE_BIN_REVIEW", "reviewer-lazy.sh")],
            "reviewed",
            "write 42"
        )
        .status
        .success()
    );
    assert_eq!(e.task(2).0, "succeeded");
    assert_eq!(
        check(&e.attempts(2)[1].4, "note", "executed-something"),
        Some(false)
    );
    // 3: a confirming reviewer.
    assert!(
        run_wf(
            &e,
            "ok.sh",
            &[("FORGE2_CLAUDE_BIN_REVIEW", "reviewer-ok.sh")],
            "reviewed",
            "write 42"
        )
        .status
        .success()
    );
    assert_eq!(e.task(3).0, "succeeded");
    // 4: a reviewer that edits the branch fails L0 and the task.
    assert!(
        !run_wf(
            &e,
            "ok.sh",
            &[("FORGE2_CLAUDE_BIN_REVIEW", "reviewer-meddles.sh")],
            "reviewed",
            "write 42"
        )
        .status
        .success()
    );
    assert_eq!(e.attempts(4)[1].2, "L0 failed: no-writes");
}

#[test]
fn the_docs_directive_is_scoped_and_cheap_uses_its_model() {
    let e = Env::new();
    // docs: writes only NOTES.md. The test repo's L1 checks require answer.txt, so give the task a --check-free repo:
    std::fs::write(
        e.repo.join("forge.toml"),
        "[checks]\nshell = [\"bash\", \"-n\", \"hello.sh\"]\n",
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "docs-friendly checks"]);
    assert!(
        run_wf(&e, "docs-ok.sh", &[], "docs", "add notes")
            .status
            .success()
    );
    assert_eq!(
        check(&e.attempts(1)[0].4, "L0", "paths-in-scope"),
        Some(true)
    );
    assert!(
        e.log_text(1, 1)
            .contains("may only change these paths: docs/, *.md")
    );
    assert!(
        !run_wf(&e, "docs-violation.sh", &[], "docs", "add notes")
            .status
            .success()
    );
    let a = e.attempts(2);
    assert_eq!(a[0].2, "L0 failed: paths-in-scope");
    assert_eq!(a[0].0, 1);
    let doc: serde_json::Value = e.trace_json("2");
    assert_eq!(doc["attempts"][0]["step"], "docs");
    assert_eq!(doc["attempts"][0]["inputs"]["step"], "docs");
    // cheap: the fix directive's model and turns reach the launch.
    assert!(
        run_wf(&e, "docs-ok.sh", &[], "cheap", "add notes")
            .status
            .success()
    );
    let doc: serde_json::Value = e.trace_json("3");
    assert_eq!(doc["attempts"][0]["inputs"]["model"], "haiku");
    assert_eq!(doc["attempts"][0]["inputs"]["max_turns"], 15);
    assert_eq!(doc["attempts"][0]["step"], "fix");
}

#[test]
fn polish_runs_a_second_code_pass_with_its_brief() {
    let e = Env::new();
    assert!(
        run_wf(
            &e,
            "ok.sh",
            &[("FORGE2_CLAUDE_BIN_POLISH", "noop.sh")],
            "polish",
            "write 42"
        )
        .status
        .success()
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert_eq!(a[1].1, "succeeded");
    let p2 = e.log_text(1, 2);
    assert!(
        p2.contains("Do not add features or scope"),
        "the brief reaches the second pass:\n{p2}"
    );
    let doc: serde_json::Value = e.trace_json("1");
    assert_eq!(doc["attempts"][1]["step"], "polish");
    assert_eq!(doc["resolved"]["steps"][2]["action"]["contract"], "code");
}

#[test]
fn a_reviewer_that_cannot_finish_leaves_the_verified_branch_for_a_human() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE2_CLAUDE_BIN_REVIEW", "crash.sh")],
        "reviewed",
        "write 42",
    );
    assert!(!o.status.success());
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "unverified", "{reason}");
    assert!(
        reason.starts_with("review could not finish (agent exit 1)"),
        "{reason}"
    );
    assert!(
        pushed,
        "the code step verified the branch; the human needs to see it"
    );
    let o = e.forge("ok.sh", &["show", "1"]);
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("only the reviewer failed to reach a verdict")
    );
}

#[test]
fn the_document_directive_is_held_to_comments_and_docs() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE2_CLAUDE_BIN_DOCUMENT", "documenter.sh")],
        "documented",
        "write 42",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, _, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(pushed);
    let ops = op_names(&e, 1);
    assert_eq!(
        ops.last().map(|(n, ok)| (n.as_str(), *ok)),
        Some(("push", true))
    );
    assert!(
        ops.iter().any(|(n, ok)| n == "comments-only" && *ok),
        "{ops:?}"
    );
    let hello = origin_file(&e, "forge/1-write-42", "hello.sh").unwrap();
    assert!(hello.contains("# prints a greeting"), "{hello}");

    // A pass that changes behavior is caught, sent back, and fails when the attempts run out.
    let mut c = e.with_role("ok.sh", "DOCUMENT", "documenter-bad.sh");
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42 again",
            "--workflow",
            "documented",
            "--retries",
            "0",
            "--no-land",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let (state, reason, _) = e.task(2);
    assert_eq!(state, "failed");
    assert!(reason.starts_with("operation comments-only (verifies) failed after 1 attempt(s): the documentation pass changed more than comments and docs"), "{reason}");
}

/// A blocked task supervised by the given fake: the coder asks its
/// question, the supervisor rules.

#[test]
fn the_investigate_directive_plans_without_writing_and_the_coder_follows_the_plan() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let o = run_wf(
        &e,
        "promptdump.sh",
        &[("FORGE2_CLAUDE_BIN_INVESTIGATE", "planner.sh")],
        "planned",
        "make the answer 42",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("plan     "), "the plan is announced: {err}");
    assert_eq!(e.task(1).0, "succeeded");
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(a[0].1, "succeeded", "the plan step: {a:?}");
    let doc: serde_json::Value = e.trace_json("1");
    let names: Vec<String> = doc["attempts"][0]["verdict"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"untouched".to_string())
            && names.contains(&"plan-names-real-paths".to_string()),
        "{names:?}"
    );
    assert!(!names.contains(&"has-commits".to_string()), "{names:?}");
    assert_eq!(
        doc["task"]["plan"]
            .as_str()
            .map(|p| p.starts_with("Plan: add answer.txt")),
        Some(true)
    );
    let coder_prompt = e.log_text(1, 2);
    assert!(
        coder_prompt.contains("Plan from the investigate step"),
        "{coder_prompt}"
    );
    assert!(
        coder_prompt.contains("Leave hello.sh as it is"),
        "{coder_prompt}"
    );
    assert_eq!(
        doc["attempts"][1]["inputs"]["plan"]
            .as_str()
            .map(|p| p.starts_with("Plan:")),
        Some(true)
    );

    // An investigator that starts implementing is refused; one that names
    // files that do not exist is refused.
    for (fake, row) in [
        ("planner-bad.sh", "untouched"),
        ("planner-lost.sh", "plan-names-real-paths"),
    ] {
        let o = run_wf(
            &e,
            "promptdump.sh",
            &[("FORGE2_CLAUDE_BIN_INVESTIGATE", fake)],
            "planned",
            "make the answer 42 again",
        );
        assert!(!o.status.success(), "{fake}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains(&format!("✗ L0 {row}")), "{fake}: {err}");
    }
    let (state, reason, _) = e.task(2);
    assert_eq!(state, "failed");
    assert!(reason.contains("L0 failed: untouched"), "{reason}");
    let (state, reason, _) = e.task(3);
    assert_eq!(state, "failed");
    assert!(reason.contains("plan-names-real-paths"), "{reason}");
}

#[test]
fn the_graph_directive_keeps_a_system_map_that_names_only_real_paths() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let o = run_wf(
        &e,
        "ok.sh",
        &[("FORGE2_CLAUDE_BIN_GRAPH", "grapher.sh")],
        "mapped",
        "write 42",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");
    let ops = op_names(&e, 1);
    assert!(
        ops.iter().any(|(n, ok)| n == "graph-check" && *ok),
        "{ops:?}"
    );
    let map = origin_file(&e, "forge/1-write-42", "docs/SYSTEM.md").unwrap();
    assert!(map.contains("```mermaid"));

    let mut c = e.with_role("ok.sh", "GRAPH", "grapher-bad.sh");
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42 again",
            "--workflow",
            "mapped",
            "--retries",
            "0",
            "--no-land",
        ])
        .output()
        .unwrap();
    assert!(!o.status.success());
    let (state, reason, _) = e.task(2);
    assert_eq!(state, "failed");
    assert!(reason.starts_with("operation graph-check (verifies) failed after 1 attempt(s): docs/SYSTEM.md names paths that do not exist"), "{reason}");
    let o = e.forge("ok.sh", &["trace", "2"]);
    let trace = String::from_utf8_lossy(&o.stdout);
    assert!(
        trace.contains("docs/missing.md"),
        "the missing path is named in the trace"
    );
    assert!(
        !trace.contains("export/import"),
        "prose with a slash is not a path"
    );
}
