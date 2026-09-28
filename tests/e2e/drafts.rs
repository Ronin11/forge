//! The draft editor end to end (docs/WORKFLOWS.md, "The draft editor"):
//! a two-step draft with one placeholder is put, files one build task on
//! the workflow's project under the initiative `workflow <name>`, is saved
//! `incomplete`, and enables itself once the action has landed.
use crate::support::*;

const DRAFT: &str = r#"{"name":"docs-lint","kind":"build","description":"lint the docs","project":"demo",
"steps":[{"action":"code"},
{"action":"lint-docs","placeholder":{"kind":"operation","inputs":"the tree","outputs":"a verdict"}}]}"#;

fn json(o: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

fn project(e: &Env) {
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo]
        )
        .status
        .success()
    );
}

#[test]
fn put_files_one_task_per_placeholder_marks_the_draft_incomplete_and_it_enables_when_the_action_lands()
 {
    let e = Env::new();
    project(&e);

    let checked = json(&e.forge_stdin("ok.sh", &["workflows", "draft", "check"], DRAFT));
    assert_eq!(checked["clean"], true, "{checked}");
    assert_eq!(checked["pending"], serde_json::json!(["lint-docs"]));

    let put = e.forge_stdin(
        "ok.sh",
        &[
            "workflows",
            "draft",
            "put",
            "docs-lint",
            "--message",
            "add docs-lint",
        ],
        DRAFT,
    );
    assert!(
        put.status.success(),
        "{}",
        String::from_utf8_lossy(&put.stderr)
    );
    let v = json(&put);
    assert_eq!(v["result"], "incomplete");
    assert_eq!(v["status"], "incomplete");
    let task_id = v["tasks"][0]["task_id"].as_i64().unwrap();

    let db = e.db();
    let (text, initiative): (String, i64) = db
        .query_row(
            "SELECT task, initiative FROM tasks WHERE id = ?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(text.starts_with("build action lint-docs with contract inputs: the tree; outputs: a verdict; kind: operation"), "{text}");
    let outcome: String = db
        .query_row(
            "SELECT outcome FROM initiatives WHERE id = ?1",
            [initiative],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "workflow docs-lint");

    // Put again files nothing new, and the workflow is not in the catalog yet.
    let again = json(&e.forge_stdin(
        "ok.sh",
        &[
            "workflows",
            "draft",
            "put",
            "docs-lint",
            "--message",
            "again",
        ],
        DRAFT,
    ));
    assert_eq!(again["filed"], serde_json::json!([]));
    assert!(!e.home.join("workflows/docs-lint.toml").exists());
    let saved = json(&e.forge(
        "ok.sh",
        &["workflows", "draft", "show", "docs-lint", "--json"],
    ));
    assert_eq!(saved["status"], "incomplete");

    // Nothing is enabled while the action is missing...
    let o = e.forge("ok.sh", &["workflows", "draft", "reconcile"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "");

    // ...and the draft enables itself once it lands.
    std::fs::write(
        e.home.join("workflows/actions/lint-docs.toml"),
        "name = \"lint-docs\"\nkind = \"operation\"\nrun = [\"true\"]\n",
    )
    .unwrap();
    let o = e.forge("ok.sh", &["workflows", "draft", "reconcile"]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("enabled docs-lint"));
    assert!(e.home.join("workflows/docs-lint.toml").exists());
    let saved = json(&e.forge(
        "ok.sh",
        &["workflows", "draft", "show", "docs-lint", "--json"],
    ));
    assert_eq!(saved["status"], "enabled");
}

#[test]
fn the_session_lints_after_every_change_and_a_clean_draft_puts_to_the_catalog() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let o = e.forge_stdin(
        "ok.sh",
        &["workflows", "draft", "quick"],
        "add code\nadd nope\nrm 2\nput add quick\n",
    );
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(out.contains("lint: clean"), "{out}");
    assert!(
        out.contains("lint: 1 problem(s)"),
        "an unknown action is annotated: {out}"
    );
    assert!(
        out.contains("\"result\":\"committed\"") || out.contains("committed"),
        "{out}"
    );
    assert!(e.home.join("workflows/quick.toml").exists());
}

#[test]
fn a_placeholder_without_a_project_is_refused_and_nothing_is_saved() {
    let e = Env::new();
    let no_project = DRAFT.replace(r#""project":"demo","#, "");
    let o = e.forge_stdin(
        "ok.sh",
        &["workflows", "draft", "put", "docs-lint", "--message", "m"],
        &no_project,
    );
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("build tasks need a project"));
    let list = json(&e.forge("ok.sh", &["workflows", "draft", "list", "--json"]));
    assert_eq!(list.as_array().unwrap().len(), 0);
}
