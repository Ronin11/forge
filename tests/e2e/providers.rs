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
    // Phase one's usage (100 in, 50+5 out) plus phase two's (20 in, 8 out),
    // both counted into the one attempt.
    assert_eq!(input_tokens, Some(120));
    // turn.completed's output_tokens and reasoning_output_tokens both sum
    // into the one output count, across both phases.
    assert_eq!(output_tokens, Some(63));

    let model: String = c
        .query_row("SELECT model FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(model, "codex-fake-model");

    let log = e.log_text(1, 1);
    let argvs: Vec<Vec<String>> = log
        .lines()
        .filter_map(|l| {
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
        .collect();
    assert_eq!(
        argvs.len(),
        2,
        "phase one (no schema) and phase two (resume, with schema): {argvs:?}"
    );
    let phase_one = &argvs[0];
    let phase_two = &argvs[1];

    for argv in [phase_one, phase_two] {
        assert!(
            argv.contains(&"--skip-git-repo-check".to_string()),
            "{argv:?}"
        );
        assert!(argv.contains(&"--json".to_string()), "{argv:?}");
        assert!(argv.contains(&"-C".to_string()), "{argv:?}");
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
    }

    // Phase one runs with no schema attached, so the model actually works.
    assert!(
        !phase_one.contains(&"--output-schema".to_string()),
        "{phase_one:?}"
    );
    assert!(!phase_one.contains(&"resume".to_string()), "{phase_one:?}");

    // Phase two resumes the thread phase one started and carries the schema.
    assert!(
        phase_two.contains(&"--output-schema".to_string()),
        "{phase_two:?}"
    );
    assert!(phase_two.contains(&"resume".to_string()), "{phase_two:?}");
    assert!(
        phase_two.contains(&"codex-fake-sess-1".to_string()),
        "{phase_two:?}"
    );

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
