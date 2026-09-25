//! docs/EXECUTION.md: outcomes in an action's contract, the `judgment`
//! field on a run workflow's directive steps, the directive share of cost
//! in `forge stats`, and the fixture gate on effects.

use crate::support::*;

const TRIAGE_ACTION: &str = r#"name = "triage"
kind = "directive"
contract = "plan"
description = "sort a message"
outcomes = ["reply", "uncertain"]
schema = '''
{"type":"object","required":["job"],"properties":{"job":{"type":"string"}}}
'''
"#;

const TRIAGE_WORKFLOW: &str = r#"name = "triage-flow"
kind = "run"
description = "a directive that picks a named outcome"

steps = [
  { action = "triage", role = "read", judgment = "an unknown sender's ask is not a pattern a rule can match" },
]

[trigger]
on = "manual"

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#;

fn new_project(e: &Env) {
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "equitizr",
                "--purpose",
                "p",
                "--repo",
                repo_s
            ],
        )
        .status
        .success()
    );
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
}

fn triage_setup(e: &Env) {
    new_project(e);
    std::fs::write(e.home.join("workflows/actions/triage.toml"), TRIAGE_ACTION).unwrap();
    std::fs::write(e.home.join("workflows/triage-flow.toml"), TRIAGE_WORKFLOW).unwrap();
}

fn start_triage(e: &Env, fake: &str) -> serde_json::Value {
    let mut c = e.with_role("ok.sh", "TRIAGE", fake);
    let o = c
        .args(["job", "start", "equitizr", "triage-flow", "--now"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let id: i64 = out
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("{out}{}", String::from_utf8_lossy(&o.stderr)));
    serde_json::from_slice(
        &e.forge("ok.sh", &["job", "show", &id.to_string(), "--json"])
            .stdout,
    )
    .unwrap()
}

#[test]
fn a_directive_step_records_the_outcome_it_returned_and_one_off_the_list_fails() {
    let e = Env::new();
    triage_setup(&e);

    let doc = start_triage(&e, "job-directive-outcome.sh");
    assert_eq!(doc["state"], "ok", "{doc}");
    assert_eq!(doc["steps"][0]["outcome"], "uncertain", "{doc}");

    let doc = start_triage(&e, "job-directive-bad-outcome.sh");
    assert_eq!(doc["state"], "failed", "an outcome off the list: {doc}");
}

#[test]
fn an_outcome_on_an_operation_is_refused_by_the_lint() {
    let e = Env::new();
    let root = tempfile::tempdir().unwrap();
    let actions = root.path().join(".forge/workflows/actions");
    std::fs::create_dir_all(&actions).unwrap();
    std::fs::write(
        actions.join("noop.toml"),
        "name = \"noop\"\nkind = \"operation\"\nrun = [\"true\"]\noutcomes = [\"reply\"]\n",
    )
    .unwrap();
    let o = e.forge(
        "ok.sh",
        &["workflows", "validate", root.path().to_str().unwrap()],
    );
    assert!(!o.status.success());
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("outcomes"), "{out}");
}

#[test]
fn a_run_workflow_directive_without_judgment_is_refused_quoting_the_rule() {
    let e = Env::new();
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    std::fs::write(e.home.join("workflows/actions/triage.toml"), TRIAGE_ACTION).unwrap();
    let bare = TRIAGE_WORKFLOW
        .replace(
            ", judgment = \"an unknown sender's ask is not a pattern a rule can match\"",
            "",
        )
        .replace("triage-flow", "candidate");
    let o = e.forge_stdin("ok.sh", &["workflows", "lint", "--stdin"], &bare);
    assert!(!o.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let msg = doc["problems"][0]["message"].as_str().unwrap();
    assert!(msg.contains("judgment"), "{msg}");
    assert!(
        msg.contains("an operation unless judgment is genuinely needed"),
        "{msg}"
    );

    let good = TRIAGE_WORKFLOW.replace("triage-flow", "candidate");
    let o = e.forge_stdin("ok.sh", &["workflows", "lint", "--stdin"], &good);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
}

#[test]
fn stats_shows_a_workflows_directive_share_of_cost() {
    let e = Env::new();
    triage_setup(&e);
    assert_eq!(start_triage(&e, "job-directive-outcome.sh")["state"], "ok");

    let doc: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["stats", "--json"]).stdout).unwrap();
    let row = doc["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["workflow"] == "triage-flow")
        .unwrap_or_else(|| panic!("no row for the run workflow: {doc}"));
    assert_eq!(row["directive_share"], 1.0, "{row}");
    assert!(row["directive_cost_usd"].as_f64().unwrap() > 0.0, "{row}");

    let list: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["workflows", "--json"]).stdout).unwrap();
    let wf = list["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "triage-flow")
        .unwrap();
    assert_eq!(wf["measured"]["directive_share"], 1.0, "{wf}");
}

const EFFECTFUL: &str = r#"name = "writer"
kind = "run"
description = "writes a file every minute"

steps = [
  { action = "write-file", effect = "file" },
]

[trigger]
on = "schedule"
cron = "* * * * *"
"#;

fn commit(e: &Env, files: &[(&str, &str)]) {
    for (rel, text) in files {
        let path = e.repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "automation"]);
}

fn job_count(e: &Env) -> usize {
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["job", "list", "--json"]).stdout).unwrap();
    rows.as_array().unwrap().len()
}

#[test]
fn an_effect_workflow_is_not_enabled_or_scheduled_until_a_fixture_passes() {
    let e = Env::new();
    new_project(&e);
    commit(&e, &[(".forge/workflows/writer.toml", EFFECTFUL)]);

    let o = e.forge("ok.sh", &["job", "enable", "equitizr", "writer"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains(".forge/fixtures/writer"), "{err}");

    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success());
    assert_eq!(job_count(&e), 0, "the scheduler's first run is refused");
    assert!(
        String::from_utf8_lossy(&o.stderr).contains(".forge/fixtures/writer"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    commit(
        &e,
        &[(
            ".forge/fixtures/writer/a.json",
            r#"{"input": {"path": "out.txt", "content": "hi"},
                "expect": {"state": "ok", "effects": [{"kind": "file", "target": "out.txt"}]}}"#,
        )],
    );
    let o = e.forge("ok.sh", &["job", "enable", "equitizr", "writer"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e.forge("ok.sh", &["work", "--once"]).status.success());
    assert_eq!(job_count(&e), 1);
}
