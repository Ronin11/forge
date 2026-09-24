use super::*;
use crate::ctx::Paths;
use crate::store::{Attempt, AttemptState, DeployTarget, Initiative, Project, Store};
use serde_json::Value;
use std::collections::BTreeMap;

fn fixture() -> (tempfile::TempDir, Forge) {
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

fn insert(f: &Forge, mut t: Task) -> Task {
    t.id = f.store.insert_task(&t).unwrap();
    f.store.update_task(&t).unwrap();
    t
}

/// Every word docs/PORTAL.md rules off the customer's page, as a key
/// substring: `PortalDoc`'s JSON must never carry one, at any depth.
fn assert_no_forbidden_keys(v: &Value) {
    const FORBIDDEN: &[&str] = &["cost", "attempt", "branch", "verdict"];
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                let lower = k.to_lowercase();
                for word in FORBIDDEN {
                    assert!(
                        !lower.contains(word),
                        "PortalDoc must not carry a {word:?}-shaped key, found {k:?}"
                    );
                }
                assert_no_forbidden_keys(val);
            }
        }
        Value::Array(items) => items.iter().for_each(assert_no_forbidden_keys),
        _ => {}
    }
}

/// A project with everything the operator's page would show — an
/// expensive attempt, a branch, a passing verdict, a real deploy —
/// must still hand the customer a document with none of it: only
/// what docs/PORTAL.md, "What they see" actually lists.
#[test]
fn portal_doc_never_carries_a_forbidden_key_even_when_the_project_has_everything() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "equitizr".into(),
            purpose: "quote requests turned into automations".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    let mut args = BTreeMap::new();
    args.insert("host".to_string(), "prod.example.com".to_string());
    f.store
        .add_deploy_target(&DeployTarget {
            project: "equitizr".into(),
            name: "prod".into(),
            repo: "/repo".into(),
            scope: None,
            method: "deploy-command".into(),
            args,
            check_cmd: "true".into(),
            on_landing: true,
            smoke_url: Some("https://prod.example.com/".into()),
        })
        .unwrap();
    let deploy_id = f
        .store
        .start_deploy("equitizr", "prod", "abc123", 100, None)
        .unwrap();
    f.store
        .finish_deploy(
            deploy_id,
            101,
            true,
            "ok",
            None,
            "",
            Some(true),
            Some(r#"{"screenshot":"screenshot.png"}"#),
            Some(true),
            Some("[]"),
        )
        .unwrap();

    let landed = insert(
        &f,
        Task {
            repo: "/repo".into(),
            task: "Make the quote text say 'usually same day'. Implementation: update src/pricing/quote.rs around line 42, then check tests/e2e/pricing.rs:88.".into(),
            base_branch: "main".into(),
            branch: "task-1-branch".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Succeeded,
            created_at: crate::unix_now(),
            finished_at: Some(crate::unix_now()),
            landed_sha: "deadbeef".into(),
            workflow: "direct".into(),
            project: Some("equitizr".into()),
            ..Default::default()
        },
    );
    f.store
        .insert_attempt(&Attempt {
            task_id: landed.id,
            attempt_no: 1,
            step: "code".into(),
            state: AttemptState::Succeeded,
            started_at: 1,
            cost_usd: Some(12.5),
            verdict_json: r#"[{"name":"tests","ok":true}]"#.into(),
            ..Default::default()
        })
        .unwrap();

    let ini_id = f
        .store
        .create_initiative(&Initiative {
            project: "equitizr".into(),
            outcome: "quoting takes one click".into(),
            stop_after_same_rule: 3,
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    insert(
        &f,
        Task {
            repo: "/repo".into(),
            task: "which price sheet should this pull from?".into(),
            base_branch: "main".into(),
            branch: "task-2-branch".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Blocked,
            reason: "needs input: which price sheet should this pull from?".into(),
            created_at: crate::unix_now(),
            workflow: "direct".into(),
            project: Some("equitizr".into()),
            initiative: Some(ini_id),
            ..Default::default()
        },
    );

    f.store
        .add_backlog("equitizr", "send a weekly summary")
        .unwrap();

    let p = f.store.project("equitizr").unwrap().unwrap();
    let doc = portal_doc(&f, &p).unwrap();
    assert_eq!(doc.deploy_targets.len(), 1);
    assert_eq!(doc.deploy_targets[0].where_it_runs, "prod.example.com");
    assert_eq!(doc.initiatives.len(), 1);
    assert_eq!(
        doc.initiatives[0].state, "waiting on you",
        "its only task is blocked on a question"
    );
    assert_eq!(doc.initiatives[0].pieces, 1);
    assert_eq!(doc.initiatives_more, 0);
    assert_eq!(doc.questions.len(), 1);
    assert_eq!(doc.landed.len(), 1);
    assert_eq!(
        doc.landed[0].text, "Make the quote text say 'usually same day'.",
        "first sentence only, no path-like tokens from the rest of the request"
    );
    assert_eq!(
        doc.landed[0].pieces, None,
        "a standalone task, not an initiative"
    );
    assert_eq!(doc.landed_more, 0);
    assert_eq!(doc.backlog.len(), 1);

    let v = serde_json::to_value(&doc).unwrap();
    assert_no_forbidden_keys(&v);
}

/// Every string value in `v`, scanned for the shapes the operator's
/// own tools carry that a customer's page must never: a slash path or
/// file extension, a dollar amount, or the words attempt, verdict,
/// branch, commit or sha. `screenshot` is excluded: an internal file
/// path the portal only ever opens server-side (see
/// `portal::screenshot_path`), never renders as text.
fn assert_no_forbidden_value_patterns(v: &Value) {
    const WORDS: &[&str] = &["attempt", "verdict", "branch", "commit", "sha"];
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k == "screenshot" {
                    continue;
                }
                assert_no_forbidden_value_patterns(val);
            }
        }
        Value::Array(items) => items.iter().for_each(assert_no_forbidden_value_patterns),
        Value::String(s) => {
            let lower = s.to_lowercase();
            for word in WORDS {
                assert!(
                    !lower.contains(word),
                    "value {s:?} carries the forbidden word {word:?}"
                );
            }
            assert!(!s.contains('$'), "value {s:?} looks like a dollar amount");
            for word in s.split_whitespace() {
                assert!(
                    !crate::render::is_path_like_word(word),
                    "value {s:?} carries a path-like token {word:?}"
                );
            }
        }
        _ => {}
    }
}

/// Equitizr's real record: a landed task whose text is thousands of
/// words of engineering instructions — file paths, line numbers, a
/// dollar budget, and the operator's own attempt/verdict/branch/
/// commit/sha vocabulary — after its first sentence. `PortalDoc` must
/// hand the customer only that first sentence, path-like tokens
/// stripped; a second landed task, filed in the customer's own words
/// (`title` set, as `forge add --title`/the concierge would), must
/// hand back exactly that title and nothing of its own operator text.
#[test]
fn portal_doc_strips_operator_language_from_equitizrs_real_record() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "equitizr".into(),
            purpose: "quote requests turned into automations".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    let engineering_instructions = format!(
        "Make the quote widget always show the annual discount. \
         Implementation: update src/pricing/discount.rs around line 154 \
         to add the annual multiplier, then check tests/e2e/pricing.rs:88 \
         for the assertion. {filler} Keep the attempt's cost under $2.50; \
         the verdict must show branch task/annual-discount landing clean \
         with commit sha abc1234def5678.",
        filler = "Typecheck, lint and tests must pass. ".repeat(50),
    );
    let untitled = insert(
        &f,
        Task {
            repo: "/repo".into(),
            task: engineering_instructions,
            base_branch: "main".into(),
            branch: "task-long-branch".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Succeeded,
            created_at: crate::unix_now(),
            finished_at: Some(1_700_000_000),
            landed_sha: "deadbeef".into(),
            workflow: "direct".into(),
            project: Some("equitizr".into()),
            ..Default::default()
        },
    );
    assert!(untitled.title.is_none());

    insert(
        &f,
        Task {
            repo: "/repo".into(),
            title: Some("Show the annual discount on every quote".into()),
            task: "internal: wire src/pricing/discount.rs into the quote flow; \
                   verdict must pass, branch task/x, commit sha deadbeef, \
                   attempt cost $9.99"
                .into(),
            base_branch: "main".into(),
            branch: "task-titled-branch".into(),
            model: "sonnet".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Succeeded,
            created_at: crate::unix_now(),
            finished_at: Some(1_700_000_100),
            landed_sha: "cafef00d".into(),
            workflow: "direct".into(),
            project: Some("equitizr".into()),
            ..Default::default()
        },
    );

    let p = f.store.project("equitizr").unwrap().unwrap();
    let doc = portal_doc(&f, &p).unwrap();
    assert_eq!(doc.landed.len(), 2);
    // Newest first: the titled task landed a hundred seconds later.
    assert_eq!(
        doc.landed[0].text,
        "Show the annual discount on every quote"
    );
    assert_eq!(
        doc.landed[1].text,
        "Make the quote widget always show the annual discount."
    );
    assert_eq!(doc.landed[0].pieces, None);
    assert_eq!(doc.landed[1].pieces, None);

    let v = serde_json::to_value(&doc).unwrap();
    assert_no_forbidden_keys(&v);
    assert_no_forbidden_value_patterns(&v);
}

/// Done merges landed initiatives and standalone landed tasks into
/// one newest-first list, an initiative's line carrying how many
/// tasks it took; past ten, the rest collapse into `landed_more`
/// rather than growing the page. Being built treats open initiatives
/// the same way (see docs/PORTAL.md).
#[test]
fn done_and_being_built_are_newest_first_and_cap_at_ten() {
    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "acme".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    // A landed initiative: two tasks, both succeeded, settled.
    let ini_id = f
        .store
        .create_initiative(&Initiative {
            project: "acme".into(),
            outcome: "checkout redesign shipped".into(),
            stop_after_same_rule: 3,
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    for n in 0..2 {
        insert(
            &f,
            Task {
                repo: "/repo".into(),
                task: format!("checkout piece {n}"),
                base_branch: "main".into(),
                branch: format!("ini-branch-{n}"),
                model: "sonnet".into(),
                max_turns: 10,
                max_attempts: 1,
                timeout_secs: 60,
                state: TaskState::Succeeded,
                created_at: crate::unix_now(),
                finished_at: Some(crate::unix_now()),
                landed_sha: format!("sha{n}"),
                workflow: "direct".into(),
                project: Some("acme".into()),
                initiative: Some(ini_id),
                ..Default::default()
            },
        );
    }
    f.store.settle_initiative(ini_id, 1_700_000_006).unwrap();

    // Twelve standalone landed tasks, oldest to newest, no initiative.
    for n in 0..12 {
        insert(
            &f,
            Task {
                repo: "/repo".into(),
                task: format!("Ship improvement number {n}."),
                base_branch: "main".into(),
                branch: format!("solo-branch-{n}"),
                model: "sonnet".into(),
                max_turns: 10,
                max_attempts: 1,
                timeout_secs: 60,
                state: TaskState::Succeeded,
                created_at: crate::unix_now(),
                finished_at: Some(1_700_000_000 + n),
                landed_sha: format!("solo{n}"),
                workflow: "direct".into(),
                project: Some("acme".into()),
                ..Default::default()
            },
        );
    }

    // Twelve open initiatives, oldest to newest by created_at.
    for n in 0..12 {
        f.store
            .create_initiative(&Initiative {
                project: "acme".into(),
                outcome: format!("open initiative {n}"),
                stop_after_same_rule: 3,
                created_at: 1_600_000_000 + n,
                ..Default::default()
            })
            .unwrap();
    }

    let p = f.store.project("acme").unwrap().unwrap();
    let doc = portal_doc(&f, &p).unwrap();

    // Done: 13 total landed items (1 initiative + 12 tasks), capped at
    // ten, three more.
    assert_eq!(doc.landed.len(), 10);
    assert_eq!(doc.landed_more, 3);
    assert_eq!(
        doc.landed[0].text, "Ship improvement number 11.",
        "newest standalone task first"
    );
    let checkout = doc
        .landed
        .iter()
        .find(|l| l.text == "checkout redesign shipped")
        .expect("the landed initiative's own line");
    assert_eq!(checkout.pieces, Some(2), "two pieces of work");

    // Being built: 12 open initiatives, capped at ten, two more.
    assert_eq!(doc.initiatives.len(), 10);
    assert_eq!(doc.initiatives_more, 2);
    assert_eq!(
        doc.initiatives[0].outcome, "open initiative 11",
        "newest open initiative first"
    );
    assert_eq!(doc.initiatives[0].pieces, 0, "no tasks filed on it yet");
}

/// "Running for you" continued: every run workflow the project's jobs
/// have used, newest job first, each capped at its last three, a
/// failure's reason cut to one line with any path-like token stripped
/// (see docs/PORTAL.md).
#[test]
fn run_workflows_list_the_last_three_jobs_each_newest_first() {
    use crate::store::{Job, JobState};

    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "acme".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    // Four jobs for "nightly-sync": only the newest three should
    // survive on its entry.
    for n in 0..4 {
        f.store
            .create_job(&Job {
                project: "acme".into(),
                workflow: "nightly-sync".into(),
                state: JobState::Ok,
                started_at: 1_700_000_000 + n,
                ..Default::default()
            })
            .unwrap();
    }
    // One failing job for "weekly-report", its reason cut from the
    // first failing check's tail, path-like tokens stripped.
    f.store
        .create_job(&Job {
            project: "acme".into(),
            workflow: "weekly-report".into(),
            state: JobState::Failed,
            started_at: 1_700_000_500,
            verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                level: "OP".into(),
                name: "send-report".into(),
                ok: false,
                tail: "could not reach src/report/send.rs:12, the mailer timed out".into(),
                ..Default::default()
            }])
            .unwrap(),
            ..Default::default()
        })
        .unwrap();
    // A needs-human job for "weekly-report" too, more recent than the
    // failure above.
    f.store
        .create_job(&Job {
            project: "acme".into(),
            workflow: "weekly-report".into(),
            state: JobState::NeedsHuman,
            started_at: 1_700_000_600,
            verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                level: "L0".into(),
                name: "budget".into(),
                ok: false,
                tail: "over the per-run budget".into(),
                ..Default::default()
            }])
            .unwrap(),
            ..Default::default()
        })
        .unwrap();

    let p = f.store.project("acme").unwrap().unwrap();
    let doc = portal_doc(&f, &p).unwrap();

    assert_eq!(doc.run_workflows.len(), 2);
    // Most recent job first, so "weekly-report" (started at 600)
    // sorts ahead of "nightly-sync" (started at 103 at the newest).
    assert_eq!(doc.run_workflows[0].name, "weekly-report");
    assert_eq!(doc.run_workflows[0].jobs.len(), 2);
    assert_eq!(doc.run_workflows[0].jobs[0].state, "needs_human");
    assert_eq!(
        doc.run_workflows[0].jobs[0].reason, None,
        "an unresolved workflow's on_failure policy can't be told apart \
         from the operator's, so the run is the operator's"
    );
    assert_eq!(doc.run_workflows[0].jobs[1].state, "failed");
    assert_eq!(
        doc.run_workflows[0].jobs[1].reason.as_deref(),
        Some("could not reach the mailer timed out"),
        "path-like token stripped from the tail"
    );

    assert_eq!(doc.run_workflows[1].name, "nightly-sync");
    assert_eq!(doc.run_workflows[1].jobs.len(), 3, "capped at three");
    assert_eq!(doc.run_workflows[1].jobs[0].started_at, 1_700_000_003);
    assert_eq!(doc.run_workflows[1].jobs[0].state, "ok");
    assert_eq!(doc.run_workflows[1].jobs[0].reason, None);

    let v = serde_json::to_value(&doc).unwrap();
    assert_no_forbidden_keys(&v);
}

/// A needs-human run's `reason` is the customer's only when the
/// question really went to this project's own contact: the resolved
/// workflow's `[limits] on_failure = "ask:contact"` *and*
/// `job::trigger_contact` actually finds someone to ask. A manual
/// `forge job start` run of an `ask:contact` workflow with no
/// `[trigger] contact` resolves no one — the job driver hands the
/// question to the operator instead (`src/job.rs`, `decide_on_failure`
/// / `trigger_contact`), so the portal must too.
#[test]
fn a_needs_human_run_is_the_operators_when_ask_contact_resolves_no_one() {
    use crate::store::{Job, JobState};

    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "acme".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    let workflows_dir = f.paths.home.join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();
    std::fs::write(
        workflows_dir.join("quote-by-text.toml"),
        "name = \"quote-by-text\"\nkind = \"run\"\ndescription = \"a customer texts a photo of a job and gets a quote back\"\nsteps = [{ action = \"fmt\" }]\n[trigger]\non = \"manual\"\n[limits]\nbudget_usd = 1.0\nper_day = 10\non_failure = \"ask:contact\"\n",
    )
    .unwrap();

    f.store
        .create_job(&Job {
            project: "acme".into(),
            workflow: "quote-by-text".into(),
            workflow_source: crate::workflows::JobSource::Catalog.as_str().to_string(),
            state: JobState::NeedsHuman,
            started_at: 1_700_000_100,
            verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                level: "OP".into(),
                name: "quoted".into(),
                ok: false,
                tail: "no price sheet entry for this job".into(),
                ..Default::default()
            }])
            .unwrap(),
            ..Default::default()
        })
        .unwrap();

    let p = f.store.project("acme").unwrap().unwrap();
    let doc = portal_doc(&f, &p).unwrap();

    let run = &doc.run_workflows[0].jobs[0];
    assert_eq!(run.state, "needs_human");
    assert_eq!(
        run.reason, None,
        "a manual run of an ask:contact workflow with no contact to ask is the operator's"
    );
}

/// The other half: a message-triggered job whose input carries `from`
/// resolves a real contact (`job::trigger_contact`'s first case), so
/// its needs-human run's `reason` is the customer's.
#[test]
fn a_needs_human_run_is_the_customers_when_a_message_triggered_job_names_a_sender() {
    use crate::store::{Job, JobState};

    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "acme".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    let workflows_dir = f.paths.home.join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();
    std::fs::write(
        workflows_dir.join("quote-by-text.toml"),
        "name = \"quote-by-text\"\nkind = \"run\"\ndescription = \"a customer texts a photo of a job and gets a quote back\"\nsteps = [{ action = \"fmt\" }]\n[trigger]\non = \"message\"\ncontact = \"customers\"\n[limits]\nbudget_usd = 1.0\nper_day = 10\non_failure = \"ask:contact\"\n",
    )
    .unwrap();

    let job_id = f
        .store
        .create_job(&Job {
            project: "acme".into(),
            workflow: "quote-by-text".into(),
            workflow_source: crate::workflows::JobSource::Catalog.as_str().to_string(),
            trigger_kind: crate::workflows::TriggerOn::Message.as_str().to_string(),
            state: JobState::NeedsHuman,
            started_at: 1_700_000_100,
            verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                level: "OP".into(),
                name: "quoted".into(),
                ok: false,
                tail: "no price sheet entry for this job".into(),
                ..Default::default()
            }])
            .unwrap(),
            ..Default::default()
        })
        .unwrap();
    let idir = crate::job::input_dir(&f, job_id);
    std::fs::create_dir_all(&idir).unwrap();
    std::fs::write(idir.join("input.json"), r#"{"from": "+15551234567"}"#).unwrap();

    let p = f.store.project("acme").unwrap().unwrap();
    let doc = portal_doc(&f, &p).unwrap();

    let run = &doc.run_workflows[0].jobs[0];
    assert_eq!(run.state, "needs_human");
    assert_eq!(
        run.reason.as_deref(),
        Some("no price sheet entry for this job"),
        "a message-triggered job whose input names a sender is the customer's"
    );
}

/// "Running for you", continued: an automation's own workflow
/// `description`, a rehearsal's runs marked `dry_run`, each run's
/// effects carried as plain sentences (`summary` only — never `kind`
/// or `target`), and a needs-human run's `reason` present only when
/// the workflow's own `[limits] on_failure` addresses the question to
/// this project's contact, `None` when it addresses the operator
/// instead (see `PortalJobRun`).
#[test]
fn running_for_you_carries_a_description_effects_a_rehearsal_flag_and_gates_the_needs_human_reason()
{
    use crate::store::{Job, JobEffect, JobState};

    let (_dir, f) = fixture();
    f.store
        .create_project(&Project {
            name: "acme".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();

    let workflows_dir = f.paths.home.join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();
    std::fs::write(
        workflows_dir.join("quote-by-text.toml"),
        "name = \"quote-by-text\"\nkind = \"run\"\ndescription = \"a customer texts a photo of a job and gets a quote back\"\nsteps = [{ action = \"fmt\" }]\n[trigger]\non = \"message\"\ncontact = \"+15550000\"\n[limits]\nbudget_usd = 1.0\nper_day = 10\non_failure = \"ask:contact\"\n",
    )
    .unwrap();
    std::fs::write(
        workflows_dir.join("nightly-sync.toml"),
        "name = \"nightly-sync\"\nkind = \"run\"\ndescription = \"syncs last night's orders into the book\"\nsteps = [{ action = \"fmt\" }]\n[trigger]\non = \"manual\"\n[limits]\nbudget_usd = 1.0\nper_day = 10\non_failure = \"ask:operator\"\n",
    )
    .unwrap();

    let quote_ok = f
        .store
        .create_job(&Job {
            project: "acme".into(),
            workflow: "quote-by-text".into(),
            workflow_source: crate::workflows::JobSource::Catalog.as_str().to_string(),
            state: JobState::Ok,
            dry_run: true,
            started_at: 1_700_000_100,
            ..Default::default()
        })
        .unwrap();
    f.store
        .append_job_effect(&JobEffect {
            id: 0,
            job_id: quote_ok,
            seq: 0,
            kind: "message".into(),
            target: "+15550000".into(),
            summary: "quoted the Hendersons' fence job at $1,240".into(),
            dry_run: true,
        })
        .unwrap();

    f.store
        .create_job(&Job {
            project: "acme".into(),
            workflow: "quote-by-text".into(),
            workflow_source: crate::workflows::JobSource::Catalog.as_str().to_string(),
            state: JobState::NeedsHuman,
            started_at: 1_700_000_200,
            verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                level: "OP".into(),
                name: "quoted".into(),
                ok: false,
                tail: "no price sheet entry for this job".into(),
                ..Default::default()
            }])
            .unwrap(),
            ..Default::default()
        })
        .unwrap();

    f.store
        .create_job(&Job {
            project: "acme".into(),
            workflow: "nightly-sync".into(),
            workflow_source: crate::workflows::JobSource::Catalog.as_str().to_string(),
            state: JobState::NeedsHuman,
            started_at: 1_700_000_300,
            verdict_json: serde_json::to_string(&[crate::checks::CheckResult {
                level: "L0".into(),
                name: "budget".into(),
                ok: false,
                tail: "over the per-run budget".into(),
                ..Default::default()
            }])
            .unwrap(),
            ..Default::default()
        })
        .unwrap();

    let p = f.store.project("acme").unwrap().unwrap();
    let doc = portal_doc(&f, &p).unwrap();

    let quote = doc
        .run_workflows
        .iter()
        .find(|w| w.name == "quote-by-text")
        .unwrap();
    assert_eq!(
        quote.description,
        "a customer texts a photo of a job and gets a quote back"
    );
    let ok_run = quote.jobs.iter().find(|j| j.state == "ok").unwrap();
    assert!(ok_run.dry_run);
    assert_eq!(
        ok_run.effects,
        vec!["quoted the Hendersons' fence job at $1,240".to_string()]
    );
    let asked_run = quote
        .jobs
        .iter()
        .find(|j| j.state == "needs_human")
        .unwrap();
    assert!(!asked_run.dry_run);
    assert_eq!(
        asked_run.reason.as_deref(),
        Some("no price sheet entry for this job"),
        "ask:contact carries the question in the customer's own terms"
    );

    let sync = doc
        .run_workflows
        .iter()
        .find(|w| w.name == "nightly-sync")
        .unwrap();
    assert_eq!(sync.description, "syncs last night's orders into the book");
    let sync_run = &sync.jobs[0];
    assert_eq!(sync_run.state, "needs_human");
    assert_eq!(
        sync_run.reason, None,
        "ask:operator addresses the operator, not the customer — no reason to leak"
    );

    let v = serde_json::to_value(&doc).unwrap();
    assert_no_forbidden_keys(&v);
}
