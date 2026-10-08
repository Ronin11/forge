//! `[projects.<name>.secrets]` values written `secret:NAME`, resolved
//! through the encrypted secret store when the job runs (docs/JOBS.md,
//! "The executor"): the step sees the stored value, a literal passes
//! through as written, the value is redacted in the step's tail, and a
//! reference that does not resolve fails the job before any step.

use crate::support::*;

const STORED: &str = "el-key-4b91d0c7e2a36f58";

const SHOW: &str = r#"name = "show-key"
kind = "operation"
description = "keeps what it was given and echoes the key on purpose"
run = ["bash", "-c", '''
printf %s "$ELEVENLABS_API_KEY" > got-key.txt
printf %s "$PLAIN_SETTING" > got-plain.txt
echo "key is $ELEVENLABS_API_KEY"
''']
"#;

/// A project with the `show-key` action, a one-step workflow and
/// `config.toml` holding `secrets`; `ELEVENLABS_API_KEY` saved in the store
/// with `forge secret set`.
fn setup(e: &Env, secrets: &str) {
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &[
                "project",
                "new",
                "exploration",
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
    std::fs::create_dir_all(e.home.join("secrets")).unwrap();
    std::fs::write(e.home.join("secrets/config.toml"), "backend = 'file'\n").unwrap();
    let set = e.forge_stdin(
        "ok.sh",
        &["secret", "set", "ELEVENLABS_API_KEY"],
        &format!("{STORED}\n"),
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    std::fs::write(
        e.home.join("config.toml"),
        format!("[projects.exploration.secrets]\n{secrets}"),
    )
    .unwrap();
    std::fs::write(e.home.join("workflows/actions/show-key.toml"), SHOW).unwrap();
    std::fs::write(
        e.home.join("workflows/elevenlabs-generate.toml"),
        r#"name = "elevenlabs-generate"
kind = "run"
description = "uses the key"

steps = [
  { action = "show-key" },
]

[trigger]
on = "manual"

[limits]
budget_usd = 1.0
per_day = 10
on_failure = "drop"
"#,
    )
    .unwrap();
}

/// `forge job start --now`; the job id and its `show --json` document.
fn run_job(e: &Env) -> (String, serde_json::Value) {
    let o = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "exploration",
            "elevenlabs-generate",
            "--now",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!String::from_utf8_lossy(&o.stdout).contains(STORED));
    assert!(!String::from_utf8_lossy(&o.stderr).contains(STORED));
    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let shown = e.forge("ok.sh", &["job", "show", &id, "--json"]);
    (id, serde_json::from_slice(&shown.stdout).unwrap())
}

#[test]
fn a_secret_reference_reaches_the_step_as_the_stored_value_and_is_redacted() {
    let e = Env::new();
    setup(
        &e,
        "ELEVENLABS_API_KEY = \"secret:ELEVENLABS_API_KEY\"\nPLAIN_SETTING = \"as-written\"\n",
    );
    let (id, doc) = run_job(&e);
    assert_eq!(doc["state"], "ok", "{doc:?}");

    let scratch = e.home.join("worktrees").join(format!("job-{id}"));
    assert_eq!(
        std::fs::read_to_string(scratch.join("got-key.txt")).unwrap(),
        STORED
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("got-plain.txt")).unwrap(),
        "as-written"
    );

    let steps = doc["steps"].as_array().unwrap();
    let step = steps.iter().find(|s| s["action"] == "show-key").unwrap();
    let tail = step["tail"].as_str().unwrap();
    assert!(
        tail.contains("key is [redacted:ELEVENLABS_API_KEY]"),
        "{tail}"
    );
    let full = std::fs::read_to_string(step["output_ref"].as_str().unwrap()).unwrap();
    assert!(!full.contains(STORED), "{full}");
    assert!(!doc.to_string().contains(STORED));
}

#[test]
fn an_unknown_secret_reference_fails_the_job_before_any_step() {
    let e = Env::new();
    setup(&e, "ELEVENLABS_API_KEY = \"secret:NOT_SAVED\"\n");
    let (id, doc) = run_job(&e);
    assert_eq!(doc["state"], "failed", "{doc:?}");
    let steps = doc["steps"].as_array().unwrap();
    assert!(
        !steps.iter().any(|s| s["action"] == "show-key"),
        "{steps:?}"
    );
    let scratch = e.home.join("worktrees").join(format!("job-{id}"));
    assert!(!scratch.join("got-key.txt").exists());

    let verdict = doc["verdict_json"].as_str().unwrap();
    assert!(verdict.contains("ELEVENLABS_API_KEY"), "{verdict}");
    assert!(verdict.contains("secret:NOT_SAVED"), "{verdict}");
    assert!(!doc.to_string().contains(STORED), "{doc:?}");
}
