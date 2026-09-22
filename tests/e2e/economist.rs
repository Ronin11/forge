//! Piece 4 of the economist (docs/ECONOMIST.md, "What is built"):
//! `experiment.toml`, beside the workflow catalog, draws a level per
//! unpinned factor for a task that names no provider and resolves no
//! workflow of its own, recorded on the task's own routing with source
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
