use super::*;

/// Fills in every `FinishAttempt` field the `role_stats` tests never
/// vary, so each test states only what it means to.
#[allow(clippy::too_many_arguments)]
fn finish_attempt(
    s: &Store,
    id: i64,
    state: AttemptState,
    num_turns: i64,
    cost_usd: f64,
    agent_ms: i64,
    verdict_json: &str,
    envelope_json: &str,
) {
    s.finish_attempt(&FinishAttempt {
        id,
        state,
        reason: String::new(),
        finished_at: Some(1),
        agent_exit: Some(0),
        timed_out: false,
        num_turns,
        tool_calls: 1,
        cost_usd: Some(cost_usd),
        agent_ms,
        commits: 0,
        files_changed: 0,
        dirty: false,
        verdict_json: verdict_json.into(),
        result_text: String::new(),
        envelope_json: envelope_json.into(),
        rl_five_hour: None,
        rl_seven_day: None,
        rl_five_hour_resets: None,
        rl_seven_day_resets: None,
        end_sha: String::new(),
        outputs_json: String::new(),
        session_id: String::new(),
        first_edit: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        early_signals: "[]".into(),
        early_near: "[]".into(),
    })
    .unwrap();
}

#[test]
fn defect_escape_counts_broke_base_and_repaired_once_each() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();

    let base_task = |started_at: i64| Task {
        repo: "r".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: started_at,
        started_at: Some(started_at),
        finished_at: Some(started_at + 1),
        workflow: "direct".into(),
        ..Default::default()
    };

    // A lands.
    let mut a = base_task(1);
    a.id = s.insert_task(&a).unwrap();
    a.landed_sha = "aaaaaaaa".into();
    s.update_task(&a).unwrap();

    // B starts from A's landed sha, and its first (and only) code
    // attempt is red on that base: an L1 row fails before B has done
    // anything of its own.
    let mut b = base_task(2);
    b.base_sha = "aaaaaaaa".into();
    b.id = s.insert_task(&b).unwrap();
    s.update_task(&b).unwrap();
    let b_attempt = Attempt {
        task_id: b.id,
        attempt_no: 1,
        step: "code".into(),
        started_at: 2,
        ..Default::default()
    };
    let b_attempt_id = s.insert_attempt(&b_attempt).unwrap();
    s.finish_attempt(&FinishAttempt {
        id: b_attempt_id,
        state: AttemptState::ChecksFailed,
        reason: "L1 failed: test".into(),
        finished_at: Some(3),
        agent_exit: Some(0),
        timed_out: false,
        num_turns: 1,
        tool_calls: 1,
        cost_usd: Some(0.0),
        agent_ms: 0,
        commits: 0,
        files_changed: 0,
        dirty: false,
        verdict_json: r#"[{"level":"L1","name":"test","ok":false,"exit":1,"ms":0,"timed_out":false,"tail":"","failing_tests":[]}]"#.into(),
        result_text: String::new(),
        envelope_json: String::new(),
        rl_five_hour: None,
        rl_seven_day: None,
        rl_five_hour_resets: None,
        rl_seven_day_resets: None,
        end_sha: String::new(),
        outputs_json: String::new(),
        session_id: String::new(),
        first_edit: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        early_signals: "[]".into(),
        early_near: "[]".into(),
    })
    .unwrap();

    // C carries a repairs reference to A.
    let mut c = base_task(4);
    c.id = s.insert_task(&c).unwrap();
    s.update_task(&c).unwrap();
    s.insert_task_ref(
        c.id,
        "repairs",
        &format!("forge://task/{}", a.id),
        "",
        "operator",
    )
    .unwrap();

    let stats = s.workflow_stats(&StatsFilter::default()).unwrap();
    assert_eq!(stats.len(), 1);
    let w = &stats[0];
    assert_eq!(w.tasks, 3);
    assert_eq!(w.landed, 1, "only A landed");
    assert_eq!(w.broke_base, 1, "A counts once for breaking B's base");
    assert_eq!(w.repaired, 1, "A counts once as repaired by C");
}

/// The git-level line-overlap attribution itself (half of a rewritten
/// task's cost, none for an untouched one) is exercised on a real
/// fixture repository in view.rs's `stats_tests`, next to the churn
/// test it shares a fixture style with. This is the SQL half: once
/// `task_repair_cost` is populated, `workflow_stats` sums it across
/// every landed task in the workflow, the same way it already sums
/// `task_churn`.
#[test]
fn workflow_stats_sums_the_repair_cost_cache_over_landed_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();

    let landed = |finished_at: i64, landed_sha: &str| Task {
        repo: "r".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: finished_at,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        workflow: "direct".into(),
        landed_sha: landed_sha.into(),
        ..Default::default()
    };
    let insert = |mut t: Task| {
        t.id = s.insert_task(&t).unwrap();
        s.update_task(&t).unwrap();
        t
    };

    let a = insert(landed(1000, "asha"));
    let b = insert(landed(2000, "bsha"));
    s.set_repair_cost_cache(a.id, 3.5, 9999).unwrap();
    s.set_repair_cost_cache(b.id, 1.5, 9999).unwrap();

    let stats = s.workflow_stats(&StatsFilter::default()).unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].repair_cost, 5.0, "3.5 + 1.5, cached per task");
}

/// Fixture: two landed tasks (one landed the ordinary way, one by hand
/// and later deployed) plus a withdrawn one, all in the same workflow
/// and project. Exercises both new metrics end to end at the store
/// level: human attention's four signals (operator answers, hand
/// landings, withdrawals, hand commits) summed and divided by landed
/// pieces, and time to live (a deploy's `finished_at` overriding a
/// task's own `landed_at` once one is tied to it).
#[test]
fn human_attention_and_time_to_live_count_hand_landing_withdrawal_and_deploy() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();
    s.create_project(&Project {
        name: "proj".into(),
        purpose: "p".into(),
        created_at: 0,
        ..Default::default()
    })
    .unwrap();

    let base = |created_at: i64| Task {
        repo: "r".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at,
        started_at: Some(created_at),
        finished_at: Some(created_at + 10),
        workflow: "direct".into(),
        workflow_hash: "h1".into(),
        project: Some("proj".into()),
        ..Default::default()
    };
    let insert = |mut t: Task| {
        t.id = s.insert_task(&t).unwrap();
        s.update_task(&t).unwrap();
        t
    };

    // Task A: landed the ordinary way, no deploy tied to it, and an
    // operator answered a question of its along the way.
    let mut a = base(1000);
    a.landed_sha = "asha".into();
    a.landed_at = Some(1100);
    let a = insert(a);
    s.insert_decision_by(a.id, "r", "q", "operator answered", "operator", "", None)
        .unwrap();

    // Task B: landed by a human's `forge land`, then deployed.
    let mut b = base(2000);
    b.landed_sha = "bsha".into();
    b.landed_at = Some(2500);
    b.hand_landed = true;
    let b = insert(b);
    s.set_hand_commits_cache(b.id, 3, 9999).unwrap();
    let deploy_id = s
        .start_deploy("proj", "prod", "bsha", 2500, Some(b.id))
        .unwrap();
    s.finish_deploy(
        deploy_id, 2600, true, "ok", None, "", None, None, None, None,
    )
    .unwrap();

    // Task C: withdrawn, never landed.
    let mut c = base(3000);
    c.state = TaskState::Withdrawn;
    c.finished_at = Some(3010);
    insert(c);

    let scope = StatsFilter::default();
    let human = s.human_attention_stats(&scope).unwrap();
    assert_eq!(human.len(), 1);
    let h = &human[0];
    assert_eq!(h.landed, 2);
    assert_eq!(h.operator_answers, 1);
    assert_eq!(h.hand_landed, 1);
    assert_eq!(h.withdrawals, 1);
    assert_eq!(
        h.hand_commits, 3,
        "cached per landed task, like repair_cost"
    );

    let human_p = s.human_attention_project_stats().unwrap();
    assert_eq!(human_p.len(), 1);
    let hp = &human_p[0];
    assert_eq!(hp.project, "proj");
    assert_eq!(hp.landed, 2);
    assert_eq!(hp.operator_answers, 1);
    assert_eq!(hp.hand_landed, 1);
    assert_eq!(hp.withdrawals, 1);
    assert_eq!(hp.hand_commits, 3);

    let mut ttls = s.task_ttls(&scope).unwrap();
    ttls.sort_by_key(|t| t.secs);
    assert_eq!(ttls.len(), 2);
    assert_eq!(ttls[0].secs, 100, "task A: landed_at - created_at");
    assert_eq!(
        ttls[1].secs, 600,
        "task B: the tied deploy's finished_at - created_at, not landed_at"
    );
}

#[test]
fn journal_control_stats_splits_code_retries_by_whether_the_journal_was_shown() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();
    let t = Task {
        repo: "r".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 4,
        timeout_secs: 1,
        ..Default::default()
    };
    let task_id = s.insert_task(&t).unwrap();

    let attempt = |attempt_no, step: &str, inputs_json: &str| Attempt {
        task_id,
        attempt_no,
        step: step.into(),
        started_at: 0,
        inputs_json: inputs_json.into(),
        ..Default::default()
    };
    let finish = |id, state, num_turns, first_edit, cost_usd| {
        s.finish_attempt(&FinishAttempt {
            id,
            state,
            reason: String::new(),
            finished_at: Some(1),
            agent_exit: Some(0),
            timed_out: false,
            num_turns,
            tool_calls: 1,
            cost_usd: Some(cost_usd),
            agent_ms: 0,
            commits: 1,
            files_changed: 1,
            dirty: false,
            verdict_json: "[]".into(),
            result_text: String::new(),
            envelope_json: String::new(),
            rl_five_hour: None,
            rl_seven_day: None,
            rl_five_hour_resets: None,
            rl_seven_day_resets: None,
            end_sha: String::new(),
            outputs_json: String::new(),
            session_id: String::new(),
            first_edit,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".into(),
            early_near: "[]".into(),
        })
        .unwrap();
    };

    // Two retries handed a journal.
    let a = s
        .insert_attempt(&attempt(
            2,
            "code",
            r#"{"journal":"earlier attempt said..."}"#,
        ))
        .unwrap();
    finish(a, AttemptState::Succeeded, 30, Some(10), 1.0);
    let b = s
        .insert_attempt(&attempt(3, "code", r#"{"journal":"more history"}"#))
        .unwrap();
    finish(b, AttemptState::ChecksFailed, 20, Some(6), 0.5);

    // One retry with no journal (absent field).
    let c = s.insert_attempt(&attempt(2, "code", "{}")).unwrap();
    finish(c, AttemptState::Succeeded, 25, None, 0.6);

    // Excluded: a first attempt (never a retry) even though it carries
    // a journal, and a non-code step's retry.
    let d = s
        .insert_attempt(&attempt(
            1,
            "code",
            r#"{"journal":"ignored, first attempt"}"#,
        ))
        .unwrap();
    finish(d, AttemptState::Succeeded, 99, Some(1), 9.0);
    let e = s
        .insert_attempt(&attempt(
            2,
            "review",
            r#"{"journal":"ignored, wrong step"}"#,
        ))
        .unwrap();
    finish(e, AttemptState::Succeeded, 99, Some(1), 9.0);

    let stats = s.journal_control_stats().unwrap();
    assert_eq!(stats.len(), 2);
    let journal = stats.iter().find(|j| j.has_journal).expect("a journal row");
    assert_eq!(journal.attempts, 2);
    assert_eq!(journal.succeeded, 1);
    assert_eq!(journal.mean_turns, 25.0);
    assert_eq!(journal.mean_first_edit, Some(8.0));
    assert_eq!(journal.mean_cost_usd, 0.75);

    let no_journal = stats
        .iter()
        .find(|j| !j.has_journal)
        .expect("a no-journal row");
    assert_eq!(no_journal.attempts, 1);
    assert_eq!(no_journal.succeeded, 1);
    assert_eq!(no_journal.mean_turns, 25.0);
    assert_eq!(
        no_journal.mean_first_edit, None,
        "the only attempt never edited"
    );
    assert_eq!(no_journal.mean_cost_usd, 0.6);
}

#[test]
fn role_stats_splits_by_provider_and_model_and_averages_within_each() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();

    let base_task = |started_at: i64| Task {
        repo: "r".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: started_at,
        started_at: Some(started_at),
        finished_at: Some(started_at + 1),
        workflow: "direct".into(),
        ..Default::default()
    };
    let attempt = |task_id, step: &str, provider: &str, model: &str| Attempt {
        task_id,
        attempt_no: 1,
        step: step.into(),
        provider: provider.into(),
        started_at: 0,
        inputs_json: format!(r#"{{"model":"{model}"}}"#),
        ..Default::default()
    };
    // A: code / anthropic / sonnet, succeeds and lands.
    let mut a = base_task(1);
    a.id = s.insert_task(&a).unwrap();
    let a1 = s
        .insert_attempt(&attempt(a.id, "code", "anthropic", "sonnet"))
        .unwrap();
    finish_attempt(&s, a1, AttemptState::Succeeded, 10, 1.0, 1000, "[]", "");
    a.landed_sha = "aaaaaaaa".into();
    s.update_task(&a).unwrap();

    // A also carries a review-step attempt: a different role, excluded
    // from landed/broke-base entirely.
    let a2 = s
        .insert_attempt(&attempt(a.id, "review", "anthropic", "sonnet"))
        .unwrap();
    finish_attempt(&s, a2, AttemptState::Succeeded, 2, 0.1, 100, "[]", "");

    // B: same code / anthropic / sonnet group, fails, never lands.
    let mut b = base_task(2);
    b.id = s.insert_task(&b).unwrap();
    let b1 = s
        .insert_attempt(&attempt(b.id, "code", "anthropic", "sonnet"))
        .unwrap();
    finish_attempt(&s, b1, AttemptState::ChecksFailed, 20, 3.0, 3000, "[]", "");
    s.update_task(&b).unwrap();

    // C: code / openai / gpt-5, its own group entirely, succeeds and lands.
    let mut c = base_task(3);
    c.id = s.insert_task(&c).unwrap();
    let c1 = s
        .insert_attempt(&attempt(c.id, "code", "openai", "gpt-5"))
        .unwrap();
    finish_attempt(&s, c1, AttemptState::Succeeded, 5, 0.5, 500, "[]", "");
    c.landed_sha = "cccccccc".into();
    s.update_task(&c).unwrap();

    // D: code / anthropic / haiku, its own group; starts from A's landed
    // sha and is red on it, so A's group counts a broke-base.
    let mut d = base_task(4);
    d.base_sha = "aaaaaaaa".into();
    d.id = s.insert_task(&d).unwrap();
    let d1 = s
        .insert_attempt(&attempt(d.id, "code", "anthropic", "haiku"))
        .unwrap();
    finish_attempt(
        &s,
        d1,
        AttemptState::ChecksFailed,
        1,
        0.0,
        0,
        r#"[{"level":"L1","name":"test","ok":false,"exit":1,"ms":0,"timed_out":false,"tail":"","failing_tests":[]}]"#,
        "",
    );
    s.update_task(&d).unwrap();

    let stats = s.role_stats().unwrap();
    assert_eq!(
        stats.len(),
        4,
        "code/anthropic/sonnet, code/openai/gpt-5, code/anthropic/haiku, review/anthropic/sonnet"
    );

    let find = |role: &str, provider: &str, model: &str| {
        stats
            .iter()
            .find(|r| r.role == role && r.provider == provider && r.model == model)
            .unwrap_or_else(|| panic!("no row for {role}/{provider}/{model}"))
    };

    let sonnet = find("code", "anthropic", "sonnet");
    assert_eq!(sonnet.attempts, 2);
    assert_eq!(sonnet.succeeded, 1);
    assert_eq!(sonnet.mean_turns, 15.0);
    assert_eq!(sonnet.mean_cost_usd, 2.0);
    assert_eq!(sonnet.mean_ms, 2000.0);
    assert_eq!(sonnet.landed, Some(1), "only A landed");
    assert_eq!(sonnet.broke_base, Some(1), "A broke D's base");

    let gpt = find("code", "openai", "gpt-5");
    assert_eq!(gpt.attempts, 1);
    assert_eq!(gpt.succeeded, 1);
    assert_eq!(gpt.mean_turns, 5.0);
    assert_eq!(gpt.mean_cost_usd, 0.5);
    assert_eq!(gpt.mean_ms, 500.0);
    assert_eq!(gpt.landed, Some(1));
    assert_eq!(gpt.broke_base, Some(0), "nothing based off C's landed sha");

    let haiku = find("code", "anthropic", "haiku");
    assert_eq!(haiku.attempts, 1);
    assert_eq!(haiku.succeeded, 0);
    assert_eq!(haiku.landed, Some(0), "D never landed");
    assert_eq!(haiku.broke_base, Some(0));

    let review = find("review", "anthropic", "sonnet");
    assert_eq!(review.attempts, 1);
    assert_eq!(review.succeeded, 1);
    assert_eq!(review.landed, None, "landed is code-only");
    assert_eq!(review.broke_base, None, "broke-base is code-only");
}

#[test]
fn role_stats_counts_an_investigate_or_interview_question_as_a_success() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();

    let mut t = Task {
        repo: "r".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Blocked,
        created_at: 1,
        workflow: "direct".into(),
        ..Default::default()
    };
    t.id = s.insert_task(&t).unwrap();

    let attempt = |step: &str| Attempt {
        task_id: t.id,
        attempt_no: 1,
        step: step.into(),
        provider: "anthropic".into(),
        started_at: 0,
        inputs_json: r#"{"model":"m"}"#.into(),
        ..Default::default()
    };
    let question = |kind: &str| {
        format!(
            r#"{{"schema_version":1,"summary":"s","needs_input":{{"question":"q","tried":"t","kind":"{kind}"}},"changes":[],"checks_run":[],"claims":[]}}"#
        )
    };
    let finish = |id, envelope_json: &str| {
        finish_attempt(
            &s,
            id,
            AttemptState::NeedsInput,
            1,
            0.0,
            0,
            "[]",
            envelope_json,
        )
    };

    // investigate: asked a plain question, no changes: a success.
    let a1 = s.insert_attempt(&attempt("investigate")).unwrap();
    finish(a1, &question("question"));

    // interview: the same.
    let a2 = s.insert_attempt(&attempt("interview")).unwrap();
    finish(a2, &question("question"));

    // investigate that needed a different workflow, not a question it
    // asked: not what this counts.
    let a3 = s.insert_attempt(&attempt("investigate")).unwrap();
    finish(a3, &question("workflow"));

    // code ending needs_input with a question: not an investigate or
    // interview role, so not counted as a success here.
    let a4 = s.insert_attempt(&attempt("code")).unwrap();
    finish(a4, &question("question"));

    let stats = s.role_stats().unwrap();
    let find = |role: &str| stats.iter().find(|r| r.role == role).unwrap();

    let investigate = find("investigate");
    assert_eq!(investigate.attempts, 2);
    assert_eq!(
        investigate.succeeded, 1,
        "the plain question counts; the workflow one does not"
    );

    let interview = find("interview");
    assert_eq!(interview.attempts, 1);
    assert_eq!(interview.succeeded, 1);

    let code = find("code");
    assert_eq!(code.attempts, 1);
    assert_eq!(
        code.succeeded, 0,
        "needs_input on a role that is not investigate/interview is not a success"
    );
}

#[test]
fn role_stats_unions_a_directive_job_step_under_its_own_role_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();

    let mut t = Task {
        repo: "r".into(),
        task: "t".into(),
        base_branch: "main".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: 1,
        workflow: "direct".into(),
        ..Default::default()
    };
    t.id = s.insert_task(&t).unwrap();
    let a1 = s
        .insert_attempt(&Attempt {
            task_id: t.id,
            attempt_no: 1,
            step: "draft-quote".into(),
            provider: "anthropic".into(),
            started_at: 0,
            inputs_json: r#"{"model":"haiku"}"#.into(),
            ..Default::default()
        })
        .unwrap();
    finish_attempt(&s, a1, AttemptState::Succeeded, 4, 0.02, 400, "[]", "");

    s.create_project(&Project {
        name: "equitizr".into(),
        purpose: "p".into(),
        created_at: 1,
        ..Default::default()
    })
    .unwrap();
    let job_id = s
        .create_job(&Job {
            project: "equitizr".into(),
            workflow: "quote-by-text".into(),
            trigger_kind: "manual".into(),
            state: JobState::Ok,
            started_at: 0,
            ..Default::default()
        })
        .unwrap();
    s.append_job_step(&JobStep {
        job_id,
        seq: 0,
        action: "draft-quote".into(),
        kind: "directive".into(),
        provider: "anthropic".into(),
        model: "haiku".into(),
        cost_usd: Some(0.03),
        started_at: 0,
        finished_at: Some(1),
        ..Default::default()
    })
    .unwrap();

    let stats = s.role_stats().unwrap();
    let draft_quote: Vec<_> = stats.iter().filter(|r| r.role == "draft-quote").collect();
    assert_eq!(
        draft_quote.len(),
        2,
        "attempt and job step, each its own row"
    );

    let attempt_row = draft_quote.iter().find(|r| r.kind == "attempt").unwrap();
    let job_row = draft_quote.iter().find(|r| r.kind == "job_step").unwrap();
    assert_eq!((attempt_row.attempts, attempt_row.mean_cost_usd), (1, 0.02));
    assert_eq!((job_row.attempts, job_row.mean_cost_usd), (1, 0.03));
    assert_eq!(
        (&*job_row.provider, &*job_row.model),
        ("anthropic", "haiku")
    );
    assert!(
        attempt_row.landed.is_none() && job_row.landed.is_none(),
        "neither role is code, and a job step never lands anyway"
    );

    let total: f64 = draft_quote.iter().map(|r| r.mean_cost_usd).sum();
    assert!((total - 0.05).abs() < 1e-9, "both costs count: {total}");
}
