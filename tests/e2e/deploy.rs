use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn deploy_targets_are_added_listed_and_forge_deploy_log_starts_empty() {
    let e = Env::new();
    let repo = e.repo.to_str().unwrap();
    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo],
        )
        .status
        .success()
    );

    // No targets, no deploys, yet.
    let targets: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(targets.as_array().unwrap().len(), 0);

    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // Add two targets.
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-user-service",
            "--arg",
            "unit=demo.service",
            "--check",
            "systemctl --user is-active demo.service",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "staging",
            "--repo",
            repo,
            "--scope",
            "web,api",
            "--method",
            "deploy-static",
            "--check",
            "curl -f https://staging.example.com/health",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // A duplicate (project, name) is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-command",
            "--check",
            "true",
        ],
    );
    assert!(!bad.status.success());

    // A target for an unknown project is refused.
    let bad = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "nope",
            "prod",
            "--repo",
            repo,
            "--method",
            "deploy-command",
            "--check",
            "true",
        ],
    );
    assert!(!bad.status.success());

    // `forge project deploy list --json` carries both, alphabetically.
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["name"], "prod");
    assert_eq!(rows[0]["method"], "deploy-user-service");
    assert_eq!(rows[0]["args"]["unit"], "demo.service");
    assert_eq!(
        rows[0]["check_cmd"],
        "systemctl --user is-active demo.service"
    );
    assert_eq!(rows[0]["on_landing"], true);
    assert_eq!(rows[0]["scope"], serde_json::Value::Null);
    assert_eq!(rows[1]["name"], "staging");
    assert_eq!(rows[1]["method"], "deploy-static");
    assert_eq!(rows[1]["on_landing"], false);
    assert_eq!(rows[1]["scope"], "[\"web\",\"api\"]");

    // The text form lists both too.
    let out = String::from_utf8_lossy(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo"])
            .stdout,
    )
    .to_string();
    assert!(out.contains("prod"), "{out}");
    assert!(out.contains("staging"), "{out}");

    // No deploy has run yet: the log is still empty.
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // Running a deploy target whose method is not built yet (step 3 of
    // docs/DEPLOY.md) fails naming it, and starts no deploy row.
    let o = e.forge("ok.sh", &["deploy", "demo", "prod"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(err.contains("deploy-user-service"), "{err}");
    let deploys: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(deploys.as_array().unwrap().len(), 0);

    // A nonexistent target is refused.
    let bad = e.forge("ok.sh", &["deploy", "demo", "nope"]);
    assert!(!bad.status.success());
}

/// A fake `rsync`: records the call, then copies its source into its
/// destination, understood either as a plain local path or `host:path`
/// (the host is only ever a label here, never dialled).
const FAKE_RSYNC: &str = r#"#!/bin/bash
echo "rsync $*" >> "$HOME/deploy-calls.log"
args=()
for a in "$@"; do
  case "$a" in
    -*) ;;
    *) args+=("$a") ;;
  esac
done
src="${args[0]}"
dest="${args[1]#*:}"
mkdir -p "$dest"
cp -a "$src"/. "$dest"/
"#;

/// A fake `ssh`: records the call, then runs the remote command locally,
/// ignoring the host, exactly as docs/DEPLOY.md's build order describes
/// testing a method: "a fake ssh on PATH" reaching "a directory on the
/// same machine".
const FAKE_SSH: &str = r#"#!/bin/bash
echo "ssh $1 $2" >> "$HOME/deploy-calls.log"
bash -c "$2"
"#;

fn write_fake(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

#[test]
fn a_deploy_that_passes_records_ok_and_a_failing_one_rolls_back_and_blocks_a_question() {
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

    // A "remote" the fake ssh/rsync actually reach: a directory on this
    // machine, exactly as docs/DEPLOY.md's build order intends.
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
            "cat flag.txt; grep -qx good flag.txt",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // Two commits: one whose check will pass, one whose check will fail.
    std::fs::write(e.repo.join("flag.txt"), "good\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "good"]);
    let good_sha = git(&e.repo, &["rev-parse", "HEAD"]);

    std::fs::write(e.repo.join("flag.txt"), "bad\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "bad"]);
    let bad_sha = git(&e.repo, &["rev-parse", "HEAD"]);

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake(&fakebin.join("rsync"), FAKE_RSYNC);
    write_fake(&fakebin.join("ssh"), FAKE_SSH);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let fakehome = e._dir.path().join("fakehome");
    std::fs::create_dir_all(&fakehome).unwrap();

    let run_deploy = |sha: &str| -> std::process::Output {
        e.cmd("ok.sh")
            .env("PATH", &path)
            .env("HOME", &fakehome)
            .args(["deploy", "demo", "prod", "--sha", sha])
            .output()
            .unwrap()
    };

    // A deploy that passes its check records ok.
    let o = run_deploy(&good_sha);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["sha"], good_sha);
    assert_eq!(rows[0]["check_ok"], true);
    assert_eq!(rows[0]["rolled_back_to"], serde_json::Value::Null);

    let calls = std::fs::read_to_string(fakehome.join("deploy-calls.log")).unwrap();
    assert!(calls.contains("rsync"), "{calls}");
    assert!(calls.contains("ssh remotebox"), "{calls}");
    assert_eq!(
        std::fs::read_to_string(remote.join("flag.txt")).unwrap(),
        "good\n"
    );

    // A deploy whose check fails rolls back to the last deploy that
    // passed, and blocks a question naming the check's output.
    let o = run_deploy(&bad_sha);
    assert!(!o.status.success());
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "demo", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let failed = &rows[0]; // newest first
    assert_eq!(failed["sha"], bad_sha);
    assert_eq!(failed["check_ok"], false);
    assert_eq!(failed["rolled_back_to"], good_sha);
    let output = failed["check_output"].as_str().unwrap();
    assert!(output.contains("bad"), "{output}");

    // The rollback actually redeployed the last passing commit.
    assert_eq!(
        std::fs::read_to_string(remote.join("flag.txt")).unwrap(),
        "good\n"
    );

    // A blocked question was filed on a new task (this project's
    // repository never had one), naming the check's output.
    let (state, reason): (String, String) = e
        .db()
        .query_row(
            "SELECT state, reason FROM tasks WHERE project = 'demo' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "blocked");
    assert!(reason.contains("rolled back to"), "{reason}");
    assert!(reason.contains("bad"), "{reason}");
}
