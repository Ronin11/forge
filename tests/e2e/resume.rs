use crate::support::*;

#[test]
fn an_attempt_that_hits_the_turn_cap_with_work_in_hand_is_resumed() {
    let e = Env::new();
    let o = e.run("turncap.sh", &["--retries", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("resume   continuing session sess-tur past the turn cap"),
        "{err}"
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].1, "agent_failed");
    assert_eq!(a[1].1, "succeeded");
    assert!(
        e.log_text(1, 2)
            .contains("ran out of turns before finishing"),
        "the continuation prompt"
    );
    let doc: serde_json::Value = e.trace_json("1");
    assert_eq!(doc["attempts"][1]["inputs"]["resumed"], "sess-turncap-1");
    let sid: String = e
        .db()
        .query_row(
            "SELECT session_id FROM attempts WHERE task_id=1 AND attempt_no=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sid, "sess-turncap-1");
    let (s1, s2): (String, String) = e.db().query_row("SELECT (SELECT start_sha FROM attempts WHERE task_id=1 AND attempt_no=1), (SELECT start_sha FROM attempts WHERE task_id=1 AND attempt_no=2)", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(
        s1, s2,
        "the resumed attempt is measured from where the capped one began"
    );
}

#[test]
fn an_attempt_that_only_explores_is_stopped_early_and_its_session_resumed() {
    // Two signs together (thirty calls with no edit; one command run five
    // times) end the run long before the cap, and the session continues
    // with a prompt that names them.
    let e = Env::new();
    let started = std::time::Instant::now();
    let o = e.run("explorer.sh", &["--retries", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the fake's half-minute sleep was cut short"
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains(
            "early    stopped: 30 tool calls with no edit; `grep -rn answer .` run 5 times"
        ),
        "{err}"
    );
    assert!(
        err.contains("resume   continuing session sess-exp after stopping it early"),
        "{err}"
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "{a:?}");
    assert_eq!(a[0].1, "agent_failed");
    assert!(
        a[0].2
            .starts_with("stopped early: 30 tool calls with no edit"),
        "{}",
        a[0].2
    );
    assert_eq!(a[1].1, "succeeded");
    let prompt = e.log_text(1, 2);
    assert!(
        prompt.contains("Forge stopped this attempt early"),
        "{prompt}"
    );
    assert!(
        prompt.contains("you have read enough") && prompt.contains("will not change"),
        "{prompt}"
    );
    assert!(
        e.log_text(1, 1).contains("forge_early_end"),
        "the stop is on the record"
    );
}

#[test]
fn an_attempt_that_hits_the_turn_cap_empty_handed_is_resumed_too() {
    // The session holds what the agent located even when the tree is
    // untouched; a fresh attempt would spend its turns finding it again.
    let e = Env::new();
    let o = e.run("turncapempty.sh", &["--retries", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("resume   continuing session sess-emp past the turn cap"),
        "{err}"
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert_eq!(
        (a[0].1.as_str(), a[1].1.as_str()),
        ("agent_failed", "succeeded")
    );
    assert!(
        e.log_text(1, 2)
            .contains("ran out of turns before changing anything"),
        "the empty-handed continuation prompt"
    );
}

#[test]
fn a_capped_attempt_that_still_returned_a_result_is_not_resumed() {
    let e = Env::new();
    let o = e.run("cappedresult.sh", &["--retries", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("resume   continuing"), "{err}");
    let a = e.attempts(1);
    assert_eq!(
        a[0].2, "L1 failed: answer",
        "the capped attempt's own result was judged"
    );
    assert_eq!(a[1].1, "succeeded");
    assert!(
        e.log_text(1, 2).contains("L1 answer"),
        "the second attempt got the check feedback, not the continuation prompt"
    );
}

#[test]
fn resume_on_failure_continues_the_same_session_after_failed_checks() {
    let e = Env::new();
    let o = e.run(
        "resumeonfail.sh",
        &["--resume-on-failure", "--retries", "1"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("resume   continuing session sess-res after failed checks"),
        "{err}"
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].1, "checks_failed");
    assert_eq!(a[0].2, "L1 failed: answer");
    assert_eq!(a[1].1, "succeeded");
    let doc: serde_json::Value = e.trace_json("1");
    assert_eq!(
        doc["attempts"][1]["inputs"]["resumed"],
        "sess-resumeonfail-1"
    );
    let sid: String = e
        .db()
        .query_row(
            "SELECT session_id FROM attempts WHERE task_id=1 AND attempt_no=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sid, "sess-resumeonfail-1");
}

#[test]
fn without_resume_on_failure_a_failed_check_starts_a_fresh_session() {
    let e = Env::new();
    let o = e.run("resumeonfail.sh", &["--retries", "1"]);
    assert!(
        !o.status.success(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.contains("resume   continuing"), "{err}");
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].1, "checks_failed");
    assert_eq!(
        a[1].1, "checks_failed",
        "without --resume the fake repeats the wrong answer instead of fixing it"
    );
}

#[test]
fn a_run_the_provider_refuses_does_not_count_and_waits_for_the_window() {
    let e = Env::new();
    // --retries 0: one attempt allowed, and the refused run must not be it.
    let o = e.run("ratelimit-hit.sh", &["--retries", "0"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("the provider refused this run; it does not count as an attempt"),
        "{err}"
    );
    assert!(
        err.contains("rate window 5h at 100%"),
        "the hold used the refusal's window: {err}"
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 2, "the refused run is recorded, then the real one");
    assert_eq!(a[0].2, "rate limited by the provider");
    assert_eq!(a[1].1, "succeeded");
    assert_eq!(e.task(1).0, "succeeded");
    let (resets, started2): (i64, i64) = e
        .db()
        .query_row(
            "SELECT (SELECT rl_five_hour_resets FROM attempts WHERE task_id=1 AND attempt_no=1),
                    (SELECT started_at FROM attempts WHERE task_id=1 AND attempt_no=2)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(
        started2 >= resets,
        "the second attempt did not start until the refusal's window reset: started {started2}, resets {resets}"
    );
}

#[test]
fn a_coder_that_commits_then_runs_out_of_turns_leaves_checked_code_for_a_human() {
    let e = Env::new();
    let o = e.run("cappedcommit.sh", &["--retries", "0"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("capped   ran out of turns after committing; the checks pass"),
        "{err}"
    );
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "unverified", "{reason}");
    assert!(
        reason.starts_with("ran out of turns after committing; the checks pass"),
        "{reason}"
    );
    assert!(pushed, "the checked branch is not thrown away");
    let show = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).to_string();
    assert!(show.contains("nothing vouches for what it did"), "{show}");
}

#[test]
fn task_budget_stops_retries() {
    let e = Env::new();
    assert!(
        !e.run("flaky.sh", &["--retries", "3", "--budget", "0.005"])
            .status
            .success()
    );
    assert_eq!(e.attempts(1).len(), 1);
    assert!(
        e.task(1).1.starts_with("task budget reached"),
        "{}",
        e.task(1).1
    );
}

#[test]
fn daily_budget_stops_the_worker() {
    let e = Env::new();
    e.add(&[]);
    e.add(&[]);
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[budget]\nper_day_usd = 0.005\n",
    )
    .unwrap();
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("daily budget reached"));
    assert_eq!(e.task(1).0, "succeeded");
    assert_eq!(e.task(2).0, "queued");
}

#[test]
fn the_worker_holds_while_a_rate_window_is_at_its_cap_and_resumes_after_the_reset() {
    let e = Env::new();
    e.add(&["--no-land"]);
    e.add(&["--no-land"]);
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[budget]\nfive_hour_max = 0.9\n",
    )
    .unwrap();
    let o = e.forge("ratelimited.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("rate window 5h at 95% (cap 90%)"), "{err}");
    assert!(err.contains("holding, 1 task(s) queued"), "{err}");
    assert_eq!(e.task(1).0, "succeeded");
    assert_eq!(
        e.task(2).0,
        "succeeded",
        "the second task ran once the window reset"
    );
    let (resets, started2): (i64, i64) = e
        .db()
        .query_row(
            "SELECT (SELECT rl_five_hour_resets FROM attempts WHERE task_id=1 AND attempt_no=1),
                    (SELECT started_at FROM attempts WHERE task_id=2 AND attempt_no=1)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(
        started2 >= resets,
        "the second task did not start until the window reset: started {started2}, resets {resets}"
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("no dollar cap"), "{out}");
    assert!(out.contains("windows 5h ≤ 90%"), "{out}");
}
