//! `--provider` selects a runner: a task on a codex-cli provider runs
//! end to end, through the same worker path as the claude fakes, and
//! records its runner/provider/model on the attempt.

use crate::support::*;
use std::path::Path;

fn codex_fake(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fakes")
        .join(name)
}

/// Write the operator config the test needs *before* any `forge` command
/// runs: `ensure_home_config` only writes the default once, so seeding it
/// first is how a test gets its own `[providers.*]` table in instead.
fn write_config(e: &Env, toml: &str) {
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(e.home.join("config.toml"), toml).unwrap();
}

#[test]
fn a_task_on_a_codex_provider_runs_end_to_end_and_records_runner_and_provider() {
    let e = Env::new();
    write_config(
        &e,
        "[providers.fake-codex]\nrunner = \"codex-cli\"\nmodel = \"codex-fake-model\"\n",
    );
    let mut cmd = e.cmd("ok.sh");
    cmd.env("FORGE2_CODEX_BIN", codex_fake("codex-ok.sh"));
    cmd.args([
        "run",
        e.repo.to_str().unwrap(),
        "write 42 to answer.txt",
        "--provider",
        "fake-codex",
        "--retries",
        "0",
    ]);
    let o = cmd.output().expect("forge run");
    eprintln!(
        "--- forge run --provider fake-codex ---\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed "), "{reason}");
    assert!(pushed);

    let c = e.db();
    let (runner, provider, cost_usd, input_tokens, output_tokens): (
        String,
        String,
        Option<f64>,
        Option<i64>,
        Option<i64>,
    ) = c
        .query_row(
            "SELECT runner, provider, cost_usd, input_tokens, output_tokens FROM attempts WHERE task_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(runner, "codex-cli");
    assert_eq!(provider, "fake-codex");
    // The local provider's price table defaults to 0: a real cost, not
    // absent, since codex itself reports none for Forge to fall back to.
    assert_eq!(cost_usd, Some(0.0));
    assert_eq!(input_tokens, Some(100));
    // turn.completed's output_tokens and reasoning_output_tokens both sum
    // into the one output count.
    assert_eq!(output_tokens, Some(55));

    let model: String = c
        .query_row("SELECT model FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(model, "codex-fake-model");

    let log = e.log_text(1, 1);
    let argv: Vec<String> = log
        .lines()
        .find_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            (v["type"] == "forge_test_argv").then(|| {
                v["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect()
            })
        })
        .expect("the fake logs its argv");
    assert!(
        argv.contains(&"--skip-git-repo-check".to_string()),
        "{argv:?}"
    );
    assert!(argv.contains(&"--json".to_string()), "{argv:?}");
    assert!(argv.contains(&"-C".to_string()), "{argv:?}");
    assert!(argv.contains(&"--output-schema".to_string()), "{argv:?}");
    assert!(argv.contains(&"-m".to_string()), "{argv:?}");
    assert!(argv.contains(&"codex-fake-model".to_string()), "{argv:?}");
    if e.sandbox_disabled() {
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "-s" && w[1] == "workspace-write"),
            "{argv:?}"
        );
    } else {
        assert!(
            argv.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()),
            "{argv:?}"
        );
    }

    // `forge trace --json` shows the same on the attempt row.
    let doc = e.trace_json(1);
    let a = &doc["attempts"][0];
    assert_eq!(a["runner"], "codex-cli");
    assert_eq!(a["provider"], "fake-codex");
}

#[test]
fn forge_providers_lists_the_configured_runner_and_model() {
    let e = Env::new();
    write_config(
        &e,
        "[providers.fake-codex]\nrunner = \"codex-cli\"\nmodel = \"codex-fake-model\"\nnotes = \"a test double\"\n",
    );
    let o = e.forge("ok.sh", &["providers", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let docs: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let names: Vec<&str> = docs
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"anthropic"), "{docs}");
    assert!(names.contains(&"fake-codex"), "{docs}");
    let fake = docs
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "fake-codex")
        .unwrap();
    assert_eq!(fake["runner"], "codex-cli");
    assert_eq!(fake["model"], "codex-fake-model");
    assert_eq!(fake["notes"], "a test double");
}

#[test]
fn an_unconfigured_provider_is_refused_at_creation() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--provider",
            "does-not-exist",
        ],
    );
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("does-not-exist"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// The engine resolves a step's provider as: task flag, then project,
/// then the operator's [roles] table, then "anthropic". This exercises
/// the operator and project layers end to end (task-flag precedence is
/// covered by `ctx::resolve_provider`'s own unit tests); every provider
/// here runs codex-cli against the same fake, so only the recorded
/// `provider` name on the attempt distinguishes which layer won.
#[test]
fn a_projects_role_wins_over_the_operators_and_a_tasks_flag_wins_over_both() {
    let e = Env::new();
    write_config(
        &e,
        "[providers.op-role]\nrunner = \"codex-cli\"\n\
         [providers.proj-role]\nrunner = \"codex-cli\"\n\
         [providers.task-flag]\nrunner = \"codex-cli\"\n\
         [roles]\ncode = \"op-role\"\n",
    );
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );
    assert!(
        e.forge(
            "ok.sh",
            &["project", "set", "demo", "--role", "code=proj-role"]
        )
        .status
        .success()
    );

    let mut cmd = e.cmd("ok.sh");
    cmd.env("FORGE2_CODEX_BIN", codex_fake("codex-ok.sh"));
    cmd.args([
        "run",
        repo,
        "write 42 to answer.txt",
        "--no-land",
        "--retries",
        "0",
    ]);
    let o = cmd.output().expect("forge run");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let provider: String = e
        .db()
        .query_row("SELECT provider FROM attempts WHERE task_id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        provider, "proj-role",
        "the project's own role wins over the operator's"
    );

    let mut cmd = e.cmd("ok.sh");
    cmd.env("FORGE2_CODEX_BIN", codex_fake("codex-ok.sh"));
    cmd.args([
        "run",
        repo,
        "write 42 to answer.txt",
        "--no-land",
        "--retries",
        "0",
        "--provider",
        "task-flag",
    ]);
    let o = cmd.output().expect("forge run");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let provider: String = e
        .db()
        .query_row("SELECT provider FROM attempts WHERE task_id=2", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        provider, "task-flag",
        "the task's own --provider wins over every role"
    );
}

/// `forge project set --role` validates the role name and the provider
/// name, and persists what it accepts.
#[test]
fn project_set_role_validates_and_persists() {
    let e = Env::new();
    write_config(&e, "[providers.devhome]\nrunner = \"codex-cli\"\n");
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    let bad_role = e.forge(
        "ok.sh",
        &["project", "set", "demo", "--role", "bogus=devhome"],
    );
    assert!(!bad_role.status.success());
    assert!(
        String::from_utf8_lossy(&bad_role.stderr).contains("bogus"),
        "{}",
        String::from_utf8_lossy(&bad_role.stderr)
    );

    let bad_provider = e.forge(
        "ok.sh",
        &["project", "set", "demo", "--role", "code=does-not-exist"],
    );
    assert!(!bad_provider.status.success());
    assert!(
        String::from_utf8_lossy(&bad_provider.stderr).contains("does-not-exist"),
        "{}",
        String::from_utf8_lossy(&bad_provider.stderr)
    );

    let ok = e.forge(
        "ok.sh",
        &["project", "set", "demo", "--role", "code=devhome"],
    );
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let show =
        String::from_utf8_lossy(&e.forge("ok.sh", &["project", "show", "demo"]).stdout).to_string();
    assert!(show.contains("code=devhome"), "{show}");
}

/// Each provider holds on its own rate window alone: a task routed to a
/// provider with no samples of its own still runs even while the
/// default (anthropic) provider's window sits at its cap from an
/// earlier attempt.
#[test]
fn a_task_routed_to_another_provider_runs_while_anthropics_window_is_at_its_cap() {
    let e = Env::new();
    write_config(
        &e,
        "[providers.fake-codex]\nrunner = \"codex-cli\"\nmodel = \"codex-fake-model\"\n",
    );
    let anthropic_task = e.add(&["--no-land"]);
    let other_task = e.add(&["--no-land", "--provider", "fake-codex"]);

    let mut cmd = e.cmd("ratelimited.sh");
    cmd.env("FORGE2_CODEX_BIN", codex_fake("codex-ok.sh"));
    cmd.args(["work", "--once"]);
    let o = cmd.output().expect("forge work");
    eprintln!(
        "--- forge work --once (anthropic capped, fake-codex free) ---\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    assert_eq!(e.task(anthropic_task).0, "succeeded");
    assert_eq!(
        e.task(other_task).0,
        "succeeded",
        "routed to a provider with no samples of its own, so anthropic's \
         capped window did not hold it"
    );

    let (five_hour, resets): (f64, i64) = e
        .db()
        .query_row(
            "SELECT rl_five_hour, rl_five_hour_resets FROM attempts WHERE provider='anthropic'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(five_hour >= 0.9, "anthropic's window is at its cap");
    // The other task's own attempt is unaffected by anthropic's hold: it
    // is not made to wait for anthropic's window to reset.
    let started: i64 = e
        .db()
        .query_row(
            "SELECT started_at FROM attempts WHERE provider='fake-codex'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        started < resets,
        "the other-provider task ran without waiting for anthropic's reset \
         (started {started}, anthropic resets {resets})"
    );
}

/// The queue-claim hold gates on the provider the task's *next agent step*
/// will actually run under, not always "code". A task on the `planned`
/// workflow's first directive is `investigate` (contract "plan"); routed
/// via `[roles]` to a provider with no samples of its own, it must be
/// claimed at once even while anthropic's window sits at its cap from an
/// earlier attempt. Its own later `code` step still resolves to anthropic
/// (no `[roles]` entry for "code"), so that step waits inside the engine
/// once anthropic is held — the bug this covers left the task unclaimable
/// at the queue instead.
#[test]
fn a_task_routed_by_role_runs_while_anthropics_window_is_at_its_cap() {
    let e = Env::new();
    write_config(
        &e,
        "[providers.fake-codex]\nrunner = \"codex-cli\"\nmodel = \"codex-fake-model\"\n\
         [roles]\nplan = \"fake-codex\"\n",
    );
    let anthropic_task = e.add(&["--no-land"]);
    let planned_task = e.add(&["--no-land", "--workflow", "planned"]);

    let mut cmd = e.cmd("ratelimited.sh");
    cmd.env("FORGE2_CODEX_BIN", codex_fake("codex-plan-ok.sh"));
    cmd.args(["work", "--once"]);
    let o = cmd.output().expect("forge work");
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    eprintln!("--- forge work --once (role-routed plan step) ---\n{stdout}{stderr}");
    assert!(o.status.success(), "{stderr}");

    assert_eq!(e.task(anthropic_task).0, "succeeded");
    assert_eq!(
        e.task(planned_task).0,
        "succeeded",
        "its first step is routed by role to a provider with no samples of its own"
    );

    // The worker's queue-level hold line (src/worker.rs) never fires for
    // the planned task: with the bug, its role always resolved to "code"
    // (-> anthropic), so it sat unclaimable behind anthropic's held window
    // and this line would appear.
    assert!(
        !stderr.contains("; holding,"),
        "the planned task should be claimed at once, never held at the queue: {stderr}"
    );
    // Its own code step (role "code" -> anthropic, no [roles] entry) still
    // waits inside the engine once anthropic's window is at its cap.
    assert!(
        stderr.contains("rate     ") && stderr.contains("; waiting"),
        "the planned task's code step should still wait inside the engine: {stderr}"
    );

    let started: i64 = e
        .db()
        .query_row(
            "SELECT started_at FROM attempts WHERE task_id=?1 ORDER BY id LIMIT 1",
            [planned_task],
            |r| r.get(0),
        )
        .unwrap();
    let resets: i64 = e
        .db()
        .query_row(
            "SELECT rl_five_hour_resets FROM attempts WHERE task_id=?1 AND provider='anthropic' ORDER BY id LIMIT 1",
            [anthropic_task],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        started < resets,
        "the planned task's first (plan) attempt started before anthropic's \
         recorded reset, so the unrelated anthropic window never held it \
         (started {started}, anthropic resets {resets})"
    );
}
