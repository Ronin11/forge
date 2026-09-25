//! Piece 4 of the economist (docs/ECONOMIST.md, "What is built"):
//! `experiment.toml`, beside the workflow catalog, draws a level per
//! unpinned factor for a task that names no provider, whatever its
//! workflow's source, recorded on the task's own routing with source
//! `"experiment"` (piece 2's `ctx::resolve_provider_routed`).

use crate::support::*;

fn write_experiment(e: &Env, toml: &str) {
    let dir = e.home.join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("experiment.toml"), toml).unwrap();
}

#[test]
fn a_queued_task_without_pins_gets_an_experiment_source_on_its_routing() {
    let e = Env::new();
    // A single-level factor draws deterministically, so the test needs
    // no second provider configured: it only has to prove the *source*
    // is "experiment", not that the drawn provider differs from the
    // built-in default.
    write_experiment(&e, "[factors.code]\nanthropic = 1.0\n");

    let o = e.run("ok.sh", &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let doc = e.trace_json(1);
    let source = doc["task"]["routing"]["code"]["provider"]["source"]
        .as_str()
        .unwrap();
    assert_eq!(source, "experiment", "{doc}");
    assert_eq!(
        doc["task"]["routing"]["code"]["provider"]["value"]
            .as_str()
            .unwrap(),
        "anthropic",
        "{doc}"
    );
    assert_eq!(
        doc["task"]["explore"]["code"].as_str().unwrap(),
        "anthropic",
        "{doc}"
    );
}

/// The draw does not care where the workflow came from: a task that names
/// its workflow (as every initiative-filed task does) still gets its
/// providers drawn. Until 2026-09-22 the draw also required the default
/// workflow, and so never fired on a real task.
#[test]
fn a_task_that_names_its_workflow_still_draws_its_providers() {
    let e = Env::new();
    write_experiment(&e, "[factors.code]\nanthropic = 1.0\n");

    let o = e.run("ok.sh", &["--workflow", "direct"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let doc = e.trace_json(1);
    assert_eq!(
        doc["task"]["routing"]["code"]["provider"]["source"]
            .as_str()
            .unwrap(),
        "experiment",
        "{doc}"
    );
    assert_eq!(
        doc["task"]["workflow_source"].as_str().unwrap_or("flag"),
        "flag",
        "{doc}"
    );
}

#[test]
fn a_role_the_project_pins_never_draws_from_the_experiment() {
    let e = Env::new();
    // The experiment would draw "openai" every time; the project pins
    // "code" to the built-in default instead, so the draw must skip it
    // entirely rather than override the pin (docs/ECONOMIST.md: "within
    // declared bounds", never against one).
    write_experiment(&e, "[factors.code]\nanthropic = 1.0\n");
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "new",
            "acme",
            "--purpose",
            "test",
            "--repo",
            e.repo.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.forge(
        "ok.sh",
        &["project", "set", "acme", "--role", "code=anthropic"],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.run("ok.sh", &["--project", "acme"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let doc = e.trace_json(1);
    let source = doc["task"]["routing"]["code"]["provider"]["source"]
        .as_str()
        .unwrap();
    assert_eq!(source, "project", "{doc}");
    assert!(
        doc["task"]["explore"]
            .as_object()
            .map(|m| !m.contains_key("code"))
            .unwrap_or(true),
        "{doc}"
    );
}

/// The argv the fake claude logged for a task's first attempt.
fn launched_model(e: &Env, task: i64) -> String {
    let log = e.log_text(task, 1);
    let argv: Vec<String> = log
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["type"] == "forge_test_argv")
        .map(|v| {
            v["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        })
        .expect("the fake logged its argv");
    let at = argv.iter().position(|a| a == "--model").expect("--model");
    argv[at + 1].clone()
}

/// A claude provider that names a model runs it (the opus arm); a task that
/// pinned `--model` on the same provider still runs its own.
#[test]
fn a_claude_providers_own_model_wins_unless_the_task_pinned_one() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[providers.anthropic-opus]\nrunner = \"claude-cli\"\nmodel = \"opus\"\n",
    )
    .unwrap();
    write_experiment(&e, "[factors.code]\nanthropic-opus = 1.0\n");

    let o = e.run("argv-ok.sh", &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(launched_model(&e, 1), "opus");
    let doc = e.trace_json(1);
    assert_eq!(
        doc["task"]["routing"]["code"]["model"]["value"], "opus",
        "{doc}"
    );
    assert_eq!(
        doc["task"]["routing"]["code"]["model"]["source"], "operator",
        "{doc}"
    );

    let o = e.run("argv-ok.sh", &["--model", "sonnet"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(launched_model(&e, 2), "sonnet");
    let doc = e.trace_json(2);
    assert_eq!(
        doc["task"]["routing"]["code"]["model"]["source"], "flag",
        "{doc}"
    );
}
