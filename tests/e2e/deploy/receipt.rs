//! The deploy receipt on a landed task (docs/DEPLOY.md, "When a deploy runs").

use super::{FAKE_SSH, write_fake, write_fake_rsync};
use crate::support::*;

/// The task's `deploy-receipt` decision row.
pub(super) fn receipt(e: &Env) -> serde_json::Value {
    let ds = e.decisions_json();
    ds.as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "deploy-receipt")
        .cloned()
        .unwrap_or_else(|| panic!("no deploy-receipt decision: {ds}"))
}

/// A first deploy whose check fails has nothing to roll back to: the
/// question is filed, and the landed task's receipt says so and links it.
#[test]
fn a_failed_on_landing_deploy_leaves_its_receipt_and_the_question_on_the_landed_task() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );
    let remote = e._dir.path().join("remote");
    let dest = remote.to_str().unwrap().to_string();
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "host=remotebox",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "command=true",
            "--check",
            "grep -qx 43 answer.txt",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();
    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("HOME", &fakehome)
        .args(["run", repo_s, "write 42", "--retries", "0"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded", "the landed commit stays landed");
    assert!(
        reason.contains("; deploy prod failed: the deploy of ")
            && reason.contains("nothing else to roll back to (question task 2)"),
        "{reason}"
    );
    let (q_state, q_reason, _) = e.task(2);
    assert_eq!(q_state, "blocked");
    assert!(
        q_reason.starts_with("needs input: the deploy of "),
        "{q_reason}"
    );
    let d = receipt(&e);
    assert!(
        d["answer"].as_str().unwrap().starts_with("failed: prod @ "),
        "{d}"
    );
    assert_eq!(d["retry_id"], 2);
    assert!(
        d["citations"].as_str().unwrap().ends_with(", task 2"),
        "{d}"
    );
}
