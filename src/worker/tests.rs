#[test]
fn a_worker_claims_only_the_slots_the_others_are_not_using() {
    use super::slot_budget;
    assert_eq!(slot_budget(4, 0), 4);
    assert_eq!(slot_budget(4, 2), 2);
    assert_eq!(slot_budget(4, 4), 0);
    assert_eq!(slot_budget(4, 9), 0, "never below zero");
    assert_eq!(slot_budget(1, 0), 1);
}

#[test]
fn this_process_is_alive() {
    assert!(super::pid_alive(std::process::id() as i64));
}

#[test]
fn pid_one_is_alive_even_when_it_is_not_ours_to_signal() {
    // As a non-root user kill(1, 0) fails with EPERM: alive all the same.
    assert!(super::pid_alive(1));
}

#[test]
fn a_reaped_child_is_dead() {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i64;
    child.wait().unwrap();
    assert!(!super::pid_alive(pid));
}

#[test]
fn worker_alive_trusts_an_empty_or_matching_start_but_not_a_stale_one() {
    let me = std::process::id() as i64;
    let mine = crate::store::start_of(me).unwrap();
    assert!(super::worker_alive(me, ""));
    assert!(super::worker_alive(me, &mine));
    assert!(!super::worker_alive(me, "not-the-recorded-start"));
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id() as i64;
    child.wait().unwrap();
    assert!(!super::worker_alive(pid, ""), "the pid itself is dead");
}

#[test]
fn zero_negative_and_out_of_range_pids_are_never_alive() {
    // kill(0, ..) and kill(-1, ..) address process groups; never ask.
    assert!(!super::pid_alive(0));
    assert!(!super::pid_alive(-1));
    assert!(!super::pid_alive(i64::from(i32::MAX) + 1));
}

/// A rate-limit sample older than the window it describes holds
/// nothing, whatever reset time it names; a fresh one at its cap does.
#[test]
fn a_five_hour_sample_older_than_five_hours_never_holds() {
    use crate::store::RateLimitSample;
    let now = 1_800_000_000;
    let stale = RateLimitSample {
        seen_at: now - 5 * 3600 - 1,
        five_hour: Some(1.0),
        seven_day: None,
        five_hour_resets: Some(now + 6 * 3600),
        seven_day_resets: None,
    };
    assert_eq!(hold_from_sample(&stale, (0.9, 0.9), now), None);
    let fresh = RateLimitSample {
        seen_at: now - 60,
        ..stale
    };
    let (msg, until) = hold_from_sample(&fresh, (0.9, 0.9), now).unwrap();
    assert_eq!(until, now + 6 * 3600);
    assert!(msg.starts_with("rate window 5h at 100%"), "{msg}");
    let reset_passed = RateLimitSample {
        five_hour_resets: Some(now - 1),
        ..fresh
    };
    assert_eq!(hold_from_sample(&reset_passed, (0.9, 0.9), now), None);
}

use super::*;
use crate::store::Store;

/// A `Forge` over a fresh, empty store in a throwaway home: enough to
/// resolve the builtin workflows `first_role` reads.
pub(super) fn fixture() -> (tempfile::TempDir, Forge) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let paths = Paths {
        worktrees: home.join("worktrees"),
        logs: home.join("logs"),
        home,
    };
    std::fs::create_dir_all(&paths.worktrees).unwrap();
    std::fs::create_dir_all(&paths.logs).unwrap();
    let store = Store::open(&paths.home.join("forge.db")).unwrap();
    let f = Forge::open_with(paths, store).unwrap();
    (dir, f)
}

pub(super) fn task_on(workflow: &str) -> Task {
    Task {
        repo: "repo".into(),
        task: "do a thing".into(),
        base_branch: "main".into(),
        model: "sonnet".into(),
        max_turns: 10,
        max_attempts: 1,
        timeout_secs: 60,
        state: TaskState::Queued,
        created_at: crate::unix_now(),
        workflow: workflow.into(),
        ..Default::default()
    }
}

#[test]
fn the_hold_line_names_the_provider_once() {
    let window = "rate window 5h at 100% (cap 90%), resets in 71m";
    assert_eq!(
        named("openai", window),
        "openai: rate window 5h at 100% (cap 90%), resets in 71m"
    );
    let login = "openai: login refused since 01:00";
    assert_eq!(named("openai", login), login);
}

#[test]
fn first_role_is_the_first_directive_steps_contract() {
    let (_dir, f) = fixture();
    assert_eq!(first_role(&f, &task_on("direct")), "code");
    assert_eq!(first_role(&f, &task_on("planned")), "plan");
}

#[test]
fn first_role_falls_back_to_code_when_the_workflow_does_not_resolve() {
    let (_dir, f) = fixture();
    assert_eq!(first_role(&f, &task_on("no-such-workflow")), "code");
    let mut t = task_on("direct");
    t.actions_json = "not json".into();
    assert_eq!(first_role(&f, &t), "code");
}

#[test]
fn first_role_is_the_next_undone_directive_on_a_resumed_task() {
    // `reviewed` resolves to setup, repo-map, code, review: seq 3 is
    // the code step. A record whose latest attempt at that seq
    // succeeded means the run already resumes past it (the same rule
    // `engine::resume_done` builds the cursor from), so the next
    // agent step, and the provider a claim is judged against, is the
    // review, not the code the task started on.
    let (_dir, f) = fixture();
    let mut t = task_on("reviewed");
    t.id = f.store.insert_task(&t).unwrap();
    assert_eq!(first_role(&f, &t), "code");
    f.store
        .insert_attempt(&crate::store::Attempt {
            task_id: t.id,
            attempt_no: 1,
            step_seq: 3,
            state: crate::store::AttemptState::Succeeded,
            started_at: crate::unix_now(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(first_role(&f, &t), "review");
}

fn git_in(dir: &Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A `fixture()` whose project `demo` has a landed repository holding
/// one message-triggered run workflow per `(name, trigger table)`
/// given, each with a noop step.
fn message_fixture(triggers: &[(&str, &str)]) -> (tempfile::TempDir, Forge) {
    let (dir, f) = fixture();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".forge/workflows/actions")).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    git_in(&repo, &["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("forge.toml"), "[checks]\nok = [\"true\"]\n").unwrap();
    std::fs::write(
        repo.join(".forge/workflows/actions/noop.toml"),
        "name = \"noop\"\nkind = \"operation\"\ndescription = \"nothing\"\nrun = [\"true\"]\n",
    )
    .unwrap();
    for (name, trigger) in triggers {
        std::fs::write(
            repo.join(format!(".forge/workflows/{name}.toml")),
            format!(
                "name = \"{name}\"\nkind = \"run\"\ndescription = \"d\"\nsteps = [{{ action = \"noop\" }}]\n\n[trigger]\n{trigger}\n\n[assert]\nok = [\"true\"]\n"
            ),
        )
        .unwrap();
    }
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-qm", "init"]);
    f.store
        .create_project(&crate::store::Project {
            name: "demo".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    f.store
        .register_repo("demo", repo.to_str().unwrap(), None)
        .unwrap();
    (dir, f)
}

fn record(f: &Forge, direction: Direction, contact: &str, text: &str) -> Message {
    let id = f
        .store
        .insert_message(crate::store::InsertMessage {
            project: "demo",
            channel: "signal",
            contact,
            direction,
            text,
            task_id: None,
        })
        .unwrap();
    f.store.message(id).unwrap().unwrap()
}

#[tokio::test]
async fn a_matching_message_starts_one_queued_job_with_the_message_as_input() {
    let (_dir, f) = message_fixture(&[("quote", "on = \"message\"\ncontact = \"alice\"")]);
    let m = record(&f, Direction::In, "alice", "a quote please");
    let started = message_triggers(&f, &m).await;
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].0, "quote");
    let job = f.store.job(started[0].1).unwrap().unwrap();
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.trigger_kind, "message");
    assert_eq!(job.trigger_ref, m.id.to_string());
    assert_eq!(job.due_at, None);
    let input = std::fs::read_to_string(
        f.paths
            .worktrees
            .join(format!("job-{}-input/input.json", job.id)),
    )
    .unwrap();
    let input: serde_json::Value = serde_json::from_str(&input).unwrap();
    assert_eq!(
        input,
        serde_json::json!({
            "from": "alice",
            "text": "a quote please",
            "at": m.at,
            "channel": "signal",
            "message_id": m.id,
        })
    );
}

#[tokio::test]
async fn the_same_message_recorded_twice_starts_one_job() {
    let (_dir, f) = message_fixture(&[("quote", "on = \"message\"\ncontact = \"*\"")]);
    let m = record(&f, Direction::In, "alice", "hi");
    assert_eq!(message_triggers(&f, &m).await.len(), 1);
    assert!(message_triggers(&f, &m).await.is_empty());
    assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
    // A different message is a different cause.
    let m2 = record(&f, Direction::In, "alice", "hi");
    assert_eq!(message_triggers(&f, &m2).await.len(), 1);
    assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 2);
}

#[tokio::test]
async fn a_non_matching_contact_starts_nothing_and_a_star_matches_anyone() {
    let (_dir, f) = message_fixture(&[
        ("only-bob", "on = \"message\"\ncontact = \"bob\""),
        ("anyone", "on = \"message\"\ncontact = \"*\""),
        ("tick", "on = \"schedule\"\ncron = \"* * * * *\""),
        ("by-hand", "on = \"manual\""),
    ]);
    let m = record(&f, Direction::In, "alice", "hi");
    let started = message_triggers(&f, &m).await;
    assert_eq!(
        started.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        vec!["anyone"]
    );
}

#[tokio::test]
async fn an_outbound_message_fires_nothing() {
    let (_dir, f) = message_fixture(&[("anyone", "on = \"message\"\ncontact = \"*\"")]);
    let m = record(&f, Direction::Out, "alice", "on it");
    assert!(message_triggers(&f, &m).await.is_empty());
    assert!(f.store.jobs(Some("demo"), None).unwrap().is_empty());
}

#[tokio::test]
async fn a_triggers_delay_makes_the_job_scheduled_from_the_messages_own_time() {
    let (_dir, f) =
        message_fixture(&[("later", "on = \"message\"\ncontact = \"*\"\ndelay = \"1h\"")]);
    let m = record(&f, Direction::In, "alice", "hi");
    let started = message_triggers(&f, &m).await;
    let job = f.store.job(started[0].1).unwrap().unwrap();
    assert_eq!(job.state, JobState::Scheduled);
    assert_eq!(job.due_at, Some(m.at + 3600));
}

#[tokio::test]
async fn a_message_triggered_job_is_recorded_at_contact_trust() {
    let (_dir, f) = message_fixture(&[("anyone", "on = \"message\"\ncontact = \"*\"")]);
    let m = record(&f, Direction::In, "alice", "hi");
    let started = message_triggers(&f, &m).await;
    assert_eq!(
        f.store.job_trust(started[0].1).unwrap(),
        Some(crate::store::Trust::Contact)
    );
}

#[tokio::test]
async fn a_project_without_a_repository_fires_nothing() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&crate::store::Project {
            name: "demo".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    let m = record(&f, Direction::In, "alice", "hi");
    assert!(message_triggers(&f, &m).await.is_empty());
}

/// Fire `demo`'s webhook `name` the way `forge job fire` does after its
/// token check: resolve the workflow, then start the job.
async fn fire(f: &Forge, name: &str, key: &str, input: &str) -> Result<(i64, bool)> {
    let (workflow, wf, source, sha) = webhook_workflow(f, "demo", name).await?;
    job::start_webhook(crate::job::StartWebhook {
        f,
        project: "demo",
        workflow: &workflow,
        landed_sha: &sha,
        wf: &wf,
        source,
        trigger_ref: key,
        input_text: input,
        trust: crate::store::Trust::Public,
    })
}

#[tokio::test]
async fn a_webhook_starts_one_queued_job_with_the_body_as_input_and_its_key_as_the_ref() {
    let (_dir, f) = message_fixture(&[
        ("ship", "on = \"webhook\"\nname = \"orders\""),
        ("other", "on = \"webhook\"\nname = \"refunds\""),
        ("by-message", "on = \"message\"\ncontact = \"*\""),
    ]);
    let (id, started) = fire(&f, "orders", "delivery-1", r#"{"order":"17"}"#)
        .await
        .unwrap();
    assert!(started);
    let job = f.store.job(id).unwrap().unwrap();
    assert_eq!(job.workflow, "ship");
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.trigger_kind, "webhook");
    assert_eq!(job.trigger_ref, "delivery-1");
    let input =
        std::fs::read_to_string(f.paths.worktrees.join(format!("job-{id}-input/input.json")))
            .unwrap();
    assert_eq!(input, r#"{"order":"17"}"#);
    assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
}

#[tokio::test]
async fn start_webhook_has_recorded_the_trust_by_the_time_it_returns() {
    let (_dir, f) = message_fixture(&[("ship", "on = \"webhook\"\nname = \"orders\"")]);
    let (id, started) = fire(&f, "orders", "delivery-1", "{}").await.unwrap();
    assert!(started);
    assert_eq!(
        f.store.job_trust(id).unwrap(),
        Some(crate::store::Trust::Public)
    );
}

#[tokio::test]
async fn a_delivery_fired_again_returns_its_job_and_starts_no_second() {
    let (_dir, f) = message_fixture(&[("ship", "on = \"webhook\"\nname = \"orders\"")]);
    let (first, started) = fire(&f, "orders", "k", "{}").await.unwrap();
    assert!(started);
    let (again, started) = fire(&f, "orders", "k", r#"{"different":"body"}"#)
        .await
        .unwrap();
    assert!(!started);
    assert_eq!(again, first);
    assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
    // Another key is another delivery.
    let (second, started) = fire(&f, "orders", "k2", "{}").await.unwrap();
    assert!(started && second != first);
    assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 2);
}

#[tokio::test]
async fn the_unique_index_backs_the_key_when_two_deliveries_race() {
    let (_dir, f) = message_fixture(&[("ship", "on = \"webhook\"\nname = \"orders\"")]);
    let (id, _) = fire(&f, "orders", "k", "{}").await.unwrap();
    let mut dup = f.store.job(id).unwrap().unwrap();
    dup.id = 0;
    assert!(f.store.create_job(&dup).is_err(), "jobs_webhook_ref");
    // A retry:N requeue carries the ref and is exempt.
    dup.retry_count = 1;
    assert!(f.store.create_job(&dup).is_ok());
}

#[tokio::test]
async fn a_webhook_nothing_or_two_workflows_claim_is_an_error() {
    let (_dir, f) = message_fixture(&[
        ("a", "on = \"webhook\"\nname = \"twice\""),
        ("b", "on = \"webhook\"\nname = \"twice\""),
        ("m", "on = \"message\"\ncontact = \"orders\""),
    ]);
    let none = webhook_workflow(&f, "demo", "orders").await.unwrap_err();
    assert!(none.to_string().contains("no run workflow"), "{none}");
    let two = webhook_workflow(&f, "demo", "twice").await.unwrap_err();
    assert!(two.to_string().contains("a, b"), "{two}");
}

#[tokio::test]
async fn a_webhook_body_that_is_not_a_json_object_starts_nothing() {
    let (_dir, f) = message_fixture(&[("ship", "on = \"webhook\"\nname = \"orders\"")]);
    assert!(fire(&f, "orders", "k", "not json").await.is_err());
    assert!(fire(&f, "orders", "k", "[1,2]").await.is_err());
    assert!(f.store.jobs(Some("demo"), None).unwrap().is_empty());
    // An empty body is an empty object.
    let (id, _) = fire(&f, "orders", "k", "  ").await.unwrap();
    let input =
        std::fs::read_to_string(f.paths.worktrees.join(format!("job-{id}-input/input.json")))
            .unwrap();
    assert_eq!(input, "{}");
}

#[tokio::test]
async fn a_webhook_triggers_delay_makes_the_job_scheduled() {
    let (_dir, f) = message_fixture(&[(
        "later",
        "on = \"webhook\"\nname = \"orders\"\ndelay = \"1h\"",
    )]);
    let before = unix_now();
    let (id, _) = fire(&f, "orders", "k", "{}").await.unwrap();
    let job = f.store.job(id).unwrap().unwrap();
    assert_eq!(job.state, JobState::Scheduled);
    assert!(job.due_at.unwrap() >= before + 3600);
}

#[tokio::test]
async fn a_schedule_tick_refused_by_per_day_records_the_refusal_once() {
    let (dir, f) = message_fixture(&[("nightly", "on = \"schedule\"\ncron = \"* * * * *\"")]);
    let repo = dir.path().join("repo");
    let file = repo.join(".forge/workflows/nightly.toml");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str("\n[limits]\nbudget_usd = 1.0\nper_day = 1\non_failure = \"drop\"\n");
    std::fs::write(&file, text).unwrap();
    git_in(&repo, &["commit", "-aqm", "cap"]);
    f.store
        .create_job(&crate::store::Job {
            project: "demo".into(),
            workflow: "nightly".into(),
            trigger_kind: "manual".into(),
            state: JobState::Ok,
            started_at: unix_now() - 3600,
            verdict_json: "[]".into(),
            ..Default::default()
        })
        .unwrap();
    let mut log = RefusalLog::default();
    for _ in 0..3 {
        let runs = tick_run_workflows(&f).await.unwrap();
        schedule_tick(&f, &runs, &mut log).await.unwrap();
    }
    assert_eq!(f.store.jobs(Some("demo"), None).unwrap().len(), 1);
    let rows = f.store.schedule_refusals().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (rows[0].project.as_str(), rows[0].workflow.as_str()),
        ("demo", "nightly")
    );
    assert_eq!(Some(rows[0].since), log.since("demo/nightly"));
    let next = rows[0].next_allowed.expect("the window rolls");
    assert!((next - (unix_now() + 23 * 3600)).abs() < 300, "{next}");
    assert!(rows[0].reason.contains("per_day limit is 1"), "{rows:?}");
}

/// One pass of the event tick, as the poll loop runs it.
async fn tick_events(f: &Forge) {
    let runs = tick_run_workflows(f).await.unwrap();
    event_tick(f, &runs).await.unwrap();
}

/// Insert a task for `project` and emit its `task_done` with `state`.
fn finish_task(f: &Forge, project: &str, state: &str) -> i64 {
    let mut t = task_on("direct");
    t.project = Some(project.into());
    t.id = f.store.insert_task(&t).unwrap();
    f.store.update_task(&t).unwrap();
    f.report.emit(
        t.id,
        Event::TaskDone {
            audience: "none",
            state,
            attempts: 1,
            cost: 0.0,
            reason: "",
            branch: "b",
            pushed: false,
            compare: None,
            remove_cmd: "",
        },
    );
    t.id
}

fn emit_job_finished(f: &Forge, workflow: &str, job_id: i64) {
    f.report.emit(
        0,
        Event::JobFinished {
            project: "demo",
            workflow,
            job_id,
            state: "ok",
            cost_usd: 0.0,
        },
    );
}

fn event_jobs(f: &Forge) -> Vec<crate::store::Job> {
    f.store
        .jobs(Some("demo"), None)
        .unwrap()
        .into_iter()
        .filter(|j| j.trigger_kind == "event")
        .collect()
}

#[tokio::test]
async fn a_superseded_or_stopping_pass_fires_no_tick() {
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    let mut log = RefusalLog::default();
    for (superseded, stopping) in [(true, false), (false, true), (true, true)] {
        let ran = run_ticks(&f, &mut log, superseded, stopping).await.unwrap();
        assert!(!ran);
        assert_eq!(f.store.event_cursor("demo", "on-done").unwrap(), None);
    }
    assert!(run_ticks(&f, &mut log, false, false).await.unwrap());
    assert!(f.store.event_cursor("demo", "on-done").unwrap().is_some());
}

#[tokio::test]
async fn a_failing_event_tick_does_not_end_the_pass() {
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    // A directory where the log should be: the event tick errors.
    let log_path = f.paths.home.join("events.jsonl");
    let _ = std::fs::remove_file(&log_path);
    std::fs::create_dir_all(&log_path).unwrap();
    let runs = tick_run_workflows(&f).await.unwrap();
    assert!(event_tick(&f, &runs).await.is_err());
    let mut refusals = RefusalLog::default();
    assert!(run_ticks(&f, &mut refusals, false, false).await.unwrap());
}

#[tokio::test]
async fn two_start_events_on_one_event_yield_one_job_and_no_error() {
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    let runs = tick_run_workflows(&f).await.unwrap();
    let run = &runs[0];
    let start = || {
        job::start_event(crate::job::StartEvent {
            f: &f,
            project: "demo",
            workflow: "on-done",
            landed_sha: &run.landed_sha,
            wf: &run.wf,
            source: run.source,
            offset: "0:42",
            at: unix_now(),
            input: "{}",
        })
    };
    let first = start().unwrap();
    assert!(first.is_some());
    assert_eq!(start().unwrap(), None);
    assert_eq!(event_jobs(&f).len(), 1);
}

#[tokio::test]
async fn a_matching_event_starts_one_queued_job_with_the_event_as_input_and_its_offset_as_the_ref()
{
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    tick_events(&f).await; // first sight: starts at the end of the log
    let before = std::fs::metadata(f.paths.home.join("events.jsonl"))
        .map(|m| m.len())
        .unwrap_or(0);
    let task = finish_task(&f, "demo", "succeeded");
    tick_events(&f).await;
    let jobs = event_jobs(&f);
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    let job = &jobs[0];
    assert_eq!(job.workflow, "on-done");
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(job.trigger_ref, format!("0:{before}"));
    assert_eq!(job.due_at, None);
    let input = std::fs::read_to_string(
        f.paths
            .worktrees
            .join(format!("job-{}-input/input.json", job.id)),
    )
    .unwrap();
    let input: serde_json::Value = serde_json::from_str(&input).unwrap();
    assert_eq!(input["type"], "task_done");
    assert_eq!(input["state"], "succeeded");
    assert_eq!(input["task"], task);
    // Ticking again, or "restarting" (nothing in memory), starts no second.
    tick_events(&f).await;
    assert_eq!(event_jobs(&f).len(), 1);
}

#[tokio::test]
async fn a_workflow_first_seen_after_events_happened_is_not_backfilled() {
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    finish_task(&f, "demo", "succeeded");
    tick_events(&f).await;
    assert!(event_jobs(&f).is_empty());
    finish_task(&f, "demo", "failed");
    tick_events(&f).await;
    assert_eq!(event_jobs(&f).len(), 1);
}

#[tokio::test]
async fn only_events_of_the_declared_type_for_the_project_start_a_job() {
    let (_dir, f) = message_fixture(&[
        ("on-done", "on = \"event\"\ntype = \"task_done\""),
        ("on-deploy", "on = \"event\"\ntype = \"deploy_finished\""),
        ("by-hand", "on = \"manual\""),
    ]);
    f.store
        .create_project(&crate::store::Project {
            name: "other".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    tick_events(&f).await;
    finish_task(&f, "other", "succeeded"); // another project's task
    f.report.emit(0, Event::Note { text: "nothing" });
    tick_events(&f).await;
    assert!(event_jobs(&f).is_empty());

    f.report.emit(
        0,
        Event::DeployFinished {
            project: "other",
            target: "prod",
            sha: "abc",
            ok: true,
            rolled_back_to: None,
        },
    );
    tick_events(&f).await;
    assert!(event_jobs(&f).is_empty(), "another project's deploy");
    f.report.emit(
        0,
        Event::DeployFinished {
            project: "demo",
            target: "prod",
            sha: "abc",
            ok: true,
            rolled_back_to: None,
        },
    );
    tick_events(&f).await;
    let jobs = event_jobs(&f);
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].workflow, "on-deploy");
}

#[tokio::test]
async fn a_jobs_own_finish_never_starts_the_workflow_that_ran_it() {
    let (_dir, f) = message_fixture(&[
        ("again", "on = \"event\"\ntype = \"job_finished\""),
        ("watcher", "on = \"event\"\ntype = \"job_finished\""),
    ]);
    tick_events(&f).await;
    emit_job_finished(&f, "again", 41);
    tick_events(&f).await;
    let jobs = event_jobs(&f);
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].workflow, "watcher", "only the other workflow fires");
    // The watcher's own job finishing starts `again`, not itself.
    emit_job_finished(&f, "watcher", jobs[0].id);
    tick_events(&f).await;
    let mut names: Vec<_> = event_jobs(&f).into_iter().map(|j| j.workflow).collect();
    names.sort();
    assert_eq!(names, vec!["again", "watcher"]);
}

#[tokio::test]
async fn the_unique_index_backs_the_offset_when_a_cursor_write_is_lost() {
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    tick_events(&f).await;
    finish_task(&f, "demo", "succeeded");
    tick_events(&f).await;
    let job = event_jobs(&f).remove(0);
    // The crash between the job and the cursor: the cursor is back at 0.
    f.store.set_event_cursor("demo", "on-done", "0:0").unwrap();
    tick_events(&f).await;
    assert_eq!(event_jobs(&f).len(), 1);
    let mut dup = job;
    dup.id = 0;
    assert!(f.store.create_job(&dup).is_err(), "jobs_event_ref");
    dup.retry_count = 1;
    assert!(f.store.create_job(&dup).is_ok(), "a retry is exempt");
}

#[tokio::test]
async fn event_generations_drain_the_tail_and_deduplicate_only_within_a_generation() {
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    tick_events(&f).await;
    let path = f.paths.home.join("events.jsonl");
    let line = "{\"type\":\"task_done\",\"project\":\"demo\",\"ts\":1}\n";
    crate::report::log::append(&path, "{}\n", u64::MAX).unwrap();
    crate::report::log::append(&path, line, 0).unwrap();
    let saved = f.store.event_cursor("demo", "on-done").unwrap().unwrap();
    tick_events(&f).await;
    assert_eq!(event_jobs(&f).len(), 1);
    f.store.set_event_cursor("demo", "on-done", &saved).unwrap();
    crate::report::log::append(&path, line, 0).unwrap();
    tick_events(&f).await;
    let jobs = event_jobs(&f);
    assert_eq!(jobs.len(), 2);
    let refs: Vec<_> = jobs
        .iter()
        .map(|j| j.trigger_ref.split_once(':').unwrap())
        .collect();
    assert_ne!(refs[0].0, refs[1].0);
    assert_eq!(refs[0].1, refs[1].1);
    tick_events(&f).await;
    assert_eq!(event_jobs(&f).len(), 2);
}

#[tokio::test]
async fn a_log_that_rolled_is_read_again_from_its_start() {
    let (_dir, f) = message_fixture(&[("on-done", "on = \"event\"\ntype = \"task_done\"")]);
    tick_events(&f).await;
    for _ in 0..3 {
        f.report.emit(
            0,
            Event::Note {
                text: "padding padding",
            },
        );
    }
    tick_events(&f).await;
    let log = f.paths.home.join("events.jsonl");
    let cursor = f.store.event_cursor("demo", "on-done").unwrap().unwrap();
    assert_eq!(
        cursor.split_once(':').unwrap().1.parse::<u64>().unwrap(),
        std::fs::metadata(&log).unwrap().len()
    );
    // The log rolls: a new, shorter file.
    std::fs::remove_file(&log).unwrap();
    finish_task(&f, "demo", "succeeded");
    assert!(
        std::fs::metadata(&log).unwrap().len()
            < cursor.split_once(':').unwrap().1.parse::<u64>().unwrap()
    );
    tick_events(&f).await;
    assert_eq!(event_jobs(&f).len(), 1);
    assert_eq!(event_jobs(&f)[0].trigger_ref, "0:0");
}

mod idle {
    use super::super::{QueueEntry, idle_reason};
    use crate::store::QueueStatus::*;

    #[test]
    fn empty_queue_and_claimable_work_say_nothing() {
        assert_eq!(idle_reason(&[]), None);
        let q: Vec<QueueEntry> = vec![(1, WaitsOnActive { dep: 9 }), (2, Claimable)];
        assert_eq!(idle_reason(&q), None);
    }

    #[test]
    fn a_stuck_queue_says_what_it_waits_on() {
        let q: Vec<QueueEntry> = vec![
            (1, WaitsOnBlocked { dep: 7 }),
            (2, WaitsOnBlocked { dep: 7 }),
            (3, WaitsOnActive { dep: 8 }),
        ];
        assert_eq!(
            idle_reason(&q).unwrap(),
            "idle: 3 queued, none claimable: 2 wait on blocked task 7, 1 on active work"
        );
        let q: Vec<QueueEntry> = vec![(1, HeldInitiative { initiative: 4 })];
        assert_eq!(
            idle_reason(&q).unwrap(),
            "idle: 1 queued, none claimable: 1 in held initiatives"
        );
    }
}

/// The words a stopped plugin's log gets after `stopped: worker exiting: `:
/// an environment fault outranks the stop it set off.
#[test]
fn an_environment_fault_is_the_exit_cause_plugins_are_told() {
    let e = anyhow::anyhow!("worktree is dirty");
    assert_eq!(
        exit_cause(Some(&e), Some("stop signal".into())),
        "environment fault: worktree is dirty"
    );
    assert_eq!(exit_cause(None, Some("stop signal".into())), "stop signal");
    assert_eq!(exit_cause(None, None), "--max-tasks reached");
}
