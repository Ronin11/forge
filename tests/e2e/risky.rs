//! docs/WORKFLOWS.md, "Risky paths": a verified change to a repository's
//! `[verify] risky` paths lands on Jev's reading of the review, else waits
//! for the operator with the branch pushed, and `forge land` lands it.

use crate::jev::fake_jev;
use crate::support::*;
use std::path::Path;

/// The e2e repository with `answer.txt` risky, pushed as the base.
fn risky_repo(e: &Env) {
    let config = std::fs::read_to_string(e.repo.join("forge.toml")).unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        format!("{config}\n[verify]\nrisky = [\"answer.txt\"]\n"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "answer.txt is risky"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
}

/// A jev provider at the fake, and nothing else in the operator's config.
fn jev_provider(e: &Env, url: &str) {
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        format!(
            "[providers.jev]\nrunner = \"jev\"\nbase_url = \"{url}/v1/systemone\"\ncloudflare_url = \"{url}/accounts/{{account_id}}/ai/run\"\n"
        ),
    )
    .unwrap();
}

/// One landing run of the task: the reviewed workflow with a confirming
/// reviewer, or the direct one.
fn run_landing(e: &Env, workflow: &str) -> std::process::Output {
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let mut c = e.cmd("ok.sh");
    c.env("TYPESAFE_API_KEY", "test-key");
    c.env(
        "FORGE_CLAUDE_BIN_REVIEW",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/reviewer-ok.sh"),
    );
    c.args([
        "run",
        e.repo.to_str().unwrap(),
        "write 42 to answer.txt",
        "--workflow",
        workflow,
        "--retries",
        "0",
    ]);
    c.output().unwrap()
}

fn decision(e: &Env, kind: &str) -> serde_json::Value {
    let ds = e.decisions_json();
    ds.as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == kind)
        .cloned()
        .unwrap_or_else(|| panic!("no {kind} decision: {ds}"))
}

#[test]
fn a_risky_change_lands_when_jev_reads_the_review_as_approving() {
    let e = Env::new();
    risky_repo(&e);
    let (url, rx) = fake_jev("approve", 0.95);
    jev_provider(&e, &url);
    let o = run_landing(&e, "reviewed");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @"), "{reason}");
    let d = decision(&e, "risky-approve");
    assert_eq!(d["answered_by"], "jev");
    assert_eq!(d["answer"], "approve (p 0.95)");
    assert_eq!(d["citations"], "paths answer.txt");
    // Jev was shown the risky path and the review's own words.
    let (_, _, body) = rx.recv().unwrap();
    let state = body["state"].as_str().unwrap();
    assert!(state.contains("paths answer.txt"), "{state}");
    assert!(
        state.contains("the change does what the task asked"),
        "{state}"
    );
    assert!(
        String::from_utf8_lossy(&o.stderr)
            .contains("risky    paths answer.txt: the review approves (p 0.95)")
    );
}

#[test]
fn a_risky_change_jev_does_not_approve_holds_pushed_and_forge_land_lands_it() {
    let e = Env::new();
    risky_repo(&e);
    let (url, _rx) = fake_jev("block", 0.9);
    jev_provider(&e, &url);
    let o = run_landing(&e, "reviewed");
    let (state, reason, pushed) = e.task(1);
    assert_eq!(
        state,
        "unverified",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        reason.starts_with(
            "held for a verdict: risky paths answer.txt; Jev put p 0.10 on the review approving it"
        ),
        "{reason}"
    );
    assert!(
        reason.ends_with("Land it with forge land 1, or forge withdraw 1"),
        "{reason}"
    );
    assert!(pushed, "the branch verified; the operator needs to see it");
    let d = decision(&e, "risky-hold");
    assert_eq!(d["answered_by"], "jev");
    assert!(
        d["answer"]
            .as_str()
            .unwrap()
            .starts_with("hold: Jev put p 0.10"),
        "{d}"
    );
    // The operator's hand lands it as it is.
    let o = e.forge("ok.sh", &["land", "1"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @"), "{reason}");
}

#[test]
fn without_a_jev_provider_a_risky_change_holds_for_the_operator() {
    let e = Env::new();
    risky_repo(&e);
    let o = run_landing(&e, "direct");
    let (state, reason, pushed) = e.task(1);
    assert_eq!(
        state,
        "unverified",
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        reason.starts_with("held for a verdict: risky paths answer.txt; no jev provider is configured to judge the review"),
        "{reason}"
    );
    assert!(pushed);
    let d = decision(&e, "risky-hold");
    assert_eq!(d["answered_by"], "kernel");
    // A plain change to a path that is not risky lands as before.
    let e2 = Env::new();
    let o = run_landing(&e2, "direct");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(e2.task(1).1.starts_with("landed main @"));
}
