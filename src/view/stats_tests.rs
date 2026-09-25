use super::*;
use crate::store::TaskState;
use crate::store::{Assessment, Attempt, AttemptState, FinishAttempt, StepStat, WorkflowStat};

#[test]
fn workflow_row_carries_named_fields_and_the_deprecated_legacy_keys() {
    let w = WorkflowStat {
        workflow: "direct".into(),
        hash: "abc123".into(),
        tasks: 1,
        succeeded: 1,
        failed: 0,
        blocked: 0,
        unverified: 0,
        cost: 2.0,
        attempts: 1,
        landed: 0,
        broke_base: 0,
        repaired: 0,
        repair_cost: 0.0,
        added_lines: 0,
        churned_lines: 0,
    };
    let row = StatsWorkflowRow::from(&w);
    let v = serde_json::to_value(&row).unwrap();
    assert_eq!(v["workflow"], "direct");
    assert_eq!(v["pieces"], 1);
    assert_eq!(v["mean_cost_usd"], 2.0);
    assert_eq!(v["cost_per_success_usd"], 2.0);
    assert!(v["cost_per_landed_usd"].is_null());
    assert_eq!(v["broke_base"], 0);
    assert!(v["broke_base_share"].is_null(), "nothing landed");
    assert_eq!(v["repaired"], 0);
    assert!(v["repaired_share"].is_null(), "nothing landed");
    assert!(v["true_cost_per_landed_usd"].is_null(), "nothing landed");
    assert!(v["churn_share"].is_null(), "nothing added yet");
    // Deprecated header-named keys stay present, flattened alongside.
    assert_eq!(v["WF"], "direct");
    assert_eq!(v["TASKS"], 1);
    assert_eq!(v["$/OK"], 2.0);
    assert!(v["$/LANDED"].is_null());
}

#[test]
fn workflow_row_shares_defect_escape_over_landed() {
    let w = WorkflowStat {
        workflow: "direct".into(),
        hash: "abc123".into(),
        tasks: 4,
        succeeded: 4,
        failed: 0,
        blocked: 0,
        unverified: 0,
        cost: 4.0,
        attempts: 4,
        landed: 4,
        broke_base: 1,
        repaired: 2,
        repair_cost: 2.0,
        added_lines: 20,
        churned_lines: 5,
    };
    let row = StatsWorkflowRow::from(&w);
    let v = serde_json::to_value(&row).unwrap();
    assert_eq!(v["broke_base"], 1);
    assert_eq!(v["broke_base_share"], 0.25);
    assert_eq!(v["repaired"], 2);
    assert_eq!(v["repaired_share"], 0.5);
    assert_eq!(v["repair_cost_usd"], 2.0);
    assert_eq!(v["true_cost_per_landed_usd"], 1.5, "(4.0 + 2.0) / 4");
    assert_eq!(v["churn_share"], 0.25, "5 / 20");
}

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
    let store = crate::store::Store::open(&paths.home.join("forge.db")).unwrap();
    let f = Forge::open_with(paths, store).unwrap();
    (dir, f)
}

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(dir.path())
        .status()
        .unwrap();
    dir
}

/// Three landings on the same fixture repository, the second rewriting
/// the only line the first added: `task_churn`'s worked example (see
/// docs/LATER.md, the delayed-cost follow-up to "Defect escape").
#[tokio::test]
async fn churn_measures_lines_a_later_landing_on_the_same_repo_rewrites() {
    let (_home, f) = fixture();
    let repo_dir = init_repo();
    let repo = repo_dir.path();

    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let c0 = crate::git::commit_all(repo, "base").await.unwrap().unwrap();
    std::fs::write(repo.join("a.txt"), "one\nALPHA\nthree\n").unwrap();
    let c1 = crate::git::commit_all(repo, "t1").await.unwrap().unwrap();
    std::fs::write(repo.join("a.txt"), "one\nBETA\nthree\n").unwrap();
    let c2 = crate::git::commit_all(repo, "t2").await.unwrap().unwrap();
    std::fs::write(repo.join("a.txt"), "one\nBETA\nthree\nfour\n").unwrap();
    let c3 = crate::git::commit_all(repo, "t3").await.unwrap().unwrap();

    let repo_s = repo.to_string_lossy().to_string();
    let landing = |base: &str, landed: &str, finished_at: i64| Task {
        repo: repo_s.clone(),
        task: "t".into(),
        base_branch: "main".into(),
        base_sha: base.to_string(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: finished_at,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        workflow: "direct".into(),
        landed_sha: landed.to_string(),
        ..Default::default()
    };
    let insert = |mut t: Task| {
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        t
    };

    let t1 = insert(landing(&c0, &c1, 1000));
    let t2 = insert(landing(&c1, &c2, 2000));
    let t3 = insert(landing(&c2, &c3, 3000));

    refresh_churn(&f).await.unwrap();

    assert_eq!(
        f.store.churn_cache(t1.id).unwrap().map(|(a, c, _)| (a, c)),
        Some((1, 1)),
        "T2 rewrote the only line T1 added"
    );
    assert_eq!(
        f.store.churn_cache(t2.id).unwrap().map(|(a, c, _)| (a, c)),
        Some((1, 0)),
        "T3 left BETA alone"
    );
    assert_eq!(
        f.store.churn_cache(t3.id).unwrap().map(|(a, c, _)| (a, c)),
        Some((1, 0)),
        "nothing has landed after T3 yet"
    );

    let raw = f
        .store
        .workflow_stats(&crate::store::StatsFilter::default())
        .unwrap();
    let stat = raw.iter().find(|w| w.workflow == "direct").unwrap();
    assert_eq!(stat.added_lines, 3);
    assert_eq!(stat.churned_lines, 1);

    let doc = stats_doc(&f, &crate::store::StatsFilter::default(), None)
        .await
        .unwrap();
    let w = doc
        .workflows
        .iter()
        .find(|w| w.workflow == "direct")
        .unwrap();
    assert_eq!(w.churn_share, Some(1.0 / 3.0));
}

/// Task 357's fixture repository, replayed for the replacement metric:
/// path overlap over-counted every task, since src/cli.rs-style files
/// are touched by nearly every landing. Line overlap does not: a later
/// landing that rewrites half of an earlier one's added lines charges
/// it half its cost, and one that only touches the same file without
/// rewriting any of its lines charges it nothing.
#[tokio::test]
async fn repair_cost_attributes_a_later_landings_cost_by_the_share_of_lines_it_rewrote() {
    let (_home, f) = fixture();
    let repo_dir = init_repo();
    let repo = repo_dir.path();

    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let c0 = crate::git::commit_all(repo, "base").await.unwrap().unwrap();
    // T adds two lines (ALPHA, BETA), removing "two".
    std::fs::write(repo.join("a.txt"), "one\nALPHA\nBETA\nthree\n").unwrap();
    let c1 = crate::git::commit_all(repo, "t").await.unwrap().unwrap();
    // L1 rewrites one of T's two added lines (ALPHA -> GAMMA) and also
    // drops "three", a line T never touched: of the two lines L1's
    // landing removed or rewrote, only one was T's.
    std::fs::write(repo.join("a.txt"), "one\nGAMMA\nBETA\n").unwrap();
    let c2 = crate::git::commit_all(repo, "l1").await.unwrap().unwrap();
    // L2 touches a.txt again but only rewrites a line neither T nor L1
    // added ("one" -> "ONE"): none of T's lines.
    std::fs::write(repo.join("a.txt"), "ONE\nGAMMA\nBETA\n").unwrap();
    let c3 = crate::git::commit_all(repo, "l2").await.unwrap().unwrap();

    let repo_s = repo.to_string_lossy().to_string();
    let landing = |base: &str, landed: &str, finished_at: i64| Task {
        repo: repo_s.clone(),
        task: "t".into(),
        base_branch: "main".into(),
        base_sha: base.to_string(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: finished_at,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        workflow: "direct".into(),
        landed_sha: landed.to_string(),
        ..Default::default()
    };
    let insert = |mut t: Task| {
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        t
    };
    let cost = |task_id, amount: f64| {
        let attempt_id = f
            .store
            .insert_attempt(&Attempt {
                task_id,
                attempt_no: 1,
                step: "code".into(),
                started_at: 0,
                ..Default::default()
            })
            .unwrap();
        f.store
            .finish_attempt(&FinishAttempt {
                id: attempt_id,
                state: AttemptState::Succeeded,
                reason: String::new(),
                finished_at: Some(1),
                agent_exit: Some(0),
                timed_out: false,
                num_turns: 1,
                tool_calls: 1,
                cost_usd: Some(amount),
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
                first_edit: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                early_signals: "[]".into(),
                early_near: "[]".into(),
                cli_cost_usd: None,
            })
            .unwrap();
    };

    let t = insert(landing(&c0, &c1, 1000));
    let l1 = insert(landing(&c1, &c2, 2000));
    let l2 = insert(landing(&c2, &c3, 3000));
    cost(l1.id, 10.0);
    cost(l2.id, 7.0);

    refresh_repair_cost(&f).await.unwrap();

    assert_eq!(
        f.store
            .line_overlap_cache(&t.landed_sha, &l1.landed_sha)
            .unwrap(),
        Some((1, 2)),
        "L1 removed or rewrote ALPHA and three; only ALPHA was T's"
    );
    assert_eq!(
        f.store
            .line_overlap_cache(&t.landed_sha, &l2.landed_sha)
            .unwrap(),
        Some((0, 1)),
        "L2 removed \"one\", which was never T's"
    );
    assert_eq!(
        f.store
            .repair_cost_cache(t.id)
            .unwrap()
            .map(|(cost, _)| cost),
        Some(5.0),
        "half of L1's $10 (1/2 of its rewritten lines were T's) plus none of L2's $7"
    );

    let raw = f
        .store
        .workflow_stats(&crate::store::StatsFilter::default())
        .unwrap();
    let stat = raw.iter().find(|w| w.workflow == "direct").unwrap();
    assert_eq!(stat.repair_cost, 5.0);
}

/// Six landed, assessed tasks whose scores rise from 3 to 8 as their
/// churn share falls from 1.0 to 0.0: the assess directive's fast
/// proxy tracking the delayed-cost measure it stands in for (see
/// docs/ACTIONS.md, "Assessment"). Sets the churn and repair-cost
/// caches directly, past their 30-day window, so `quality_correlation`
/// reads cached numbers rather than exercising `refresh_churn`'s git
/// diff (already covered above).
#[test]
fn quality_correlation_is_negative_when_score_rises_as_churn_falls() {
    let (_home, f) = fixture();
    let landing = |finished_at: i64| Task {
        repo: "/does/not/matter".into(),
        task: "t".into(),
        base_branch: "main".into(),
        base_sha: "base".into(),
        model: "m".into(),
        max_turns: 1,
        max_attempts: 1,
        timeout_secs: 1,
        state: TaskState::Succeeded,
        created_at: finished_at,
        started_at: Some(finished_at),
        finished_at: Some(finished_at),
        workflow: "direct".into(),
        landed_sha: format!("landed-{finished_at}"),
        ..Default::default()
    };
    for (i, (score, added, churned)) in [
        (3i64, 10i64, 10i64),
        (4, 10, 8),
        (5, 10, 6),
        (6, 10, 4),
        (7, 10, 2),
        (8, 10, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let mut t = landing(1000 + i as i64);
        t.id = f.store.insert_task(&t).unwrap();
        f.store.update_task(&t).unwrap();
        f.store
            .insert_assessment(&Assessment {
                id: 0,
                task_id: t.id,
                score,
                findings_json: "[]".into(),
                model: "m".into(),
                provider: "p".into(),
                cost_usd: None,
                created_at: t.finished_at.unwrap(),
            })
            .unwrap();
        let computed_at = t.finished_at.unwrap() + crate::store::THIRTY_DAYS_SECS;
        f.store
            .set_churn_cache(t.id, added, churned, computed_at)
            .unwrap();
        f.store
            .set_repair_cost_cache(t.id, 0.0, computed_at)
            .unwrap();
    }

    let rows = quality_correlation(&f, &crate::store::StatsFilter::default()).unwrap();
    let churn = rows.iter().find(|r| r.measure == "churn").unwrap();
    assert_eq!(churn.n, 6);
    assert!(
        churn.rho.unwrap() < 0.0,
        "score rises as churn falls, expected a negative rho: {:?}",
        churn.rho
    );
}

#[test]
fn step_row_carries_named_fields_and_the_deprecated_legacy_keys() {
    let st = StepStat {
        workflow: "direct".into(),
        step: "code".into(),
        attempts: 1,
        succeeded: 1,
        agent_failed: 0,
        checks_failed: 0,
        needs_input: 0,
        mean_turns: 3.0,
        cost: 2.0,
        mean_ms: 4000.0,
        mean_first_edit: Some(1.5),
        mean_input_tokens: None,
    };
    let row = StatsStepRow::from(&st);
    let v = serde_json::to_value(&row).unwrap();
    assert_eq!(v["step"], "code");
    assert_eq!(v["mean_secs"], 4.0);
    assert_eq!(v["mean_first_edit"], 1.5);
    assert!(v["mean_input_tokens"].is_null());
    assert_eq!(v["STEP"], "code");
    assert_eq!(v["SECS"], 4.0);
    assert_eq!(v["EDIT@"], 1.5);
    assert!(v["TOKENS"].is_null());
}

#[test]
fn stats_doc_omits_tools_when_not_requested() {
    let doc = StatsDoc {
        workflows: vec![],
        steps: vec![],
        journal: StatsJournalRow::default(),
        no_journal: StatsJournalRow::default(),
        projects: vec![],
        jobs: vec![],
        by_role: vec![],
        assessment_correlation: vec![],
        human_attention: vec![],
        human_attention_projects: vec![],
        time_to_live: vec![],
        time_to_live_projects: vec![],
        factors: vec![],
        daily: vec![],
        tools: None,
    };
    let v = serde_json::to_value(&doc).unwrap();
    assert!(v.get("tools").is_none(), "{v}");
    assert!(v.get("projects").is_none(), "{v}");
    assert!(v.get("jobs").is_none(), "{v}");
    assert!(v.get("human_attention_projects").is_none(), "{v}");
    assert!(v.get("time_to_live_projects").is_none(), "{v}");
}

#[test]
fn journal_row_computes_succeeded_share_and_carries_options_through() {
    let j = JournalStat {
        has_journal: true,
        attempts: 4,
        succeeded: 3,
        mean_turns: 25.0,
        mean_first_edit: Some(8.0),
        mean_cost_usd: 0.75,
    };
    let row = StatsJournalRow::from(&j);
    let v = serde_json::to_value(&row).unwrap();
    assert_eq!(v["attempts"], 4);
    assert_eq!(v["succeeded"], 3);
    assert_eq!(v["succeeded_share"], 0.75);
    assert_eq!(v["mean_turns"], 25.0);
    assert_eq!(v["mean_first_edit"], 8.0);
    assert_eq!(v["mean_cost_usd"], 0.75);

    let empty = StatsJournalRow::default();
    let v = serde_json::to_value(&empty).unwrap();
    assert!(v["succeeded_share"].is_null());
    assert!(v["mean_first_edit"].is_null());
}
