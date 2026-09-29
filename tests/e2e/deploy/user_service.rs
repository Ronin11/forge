//! REVIEW-4 E3-18: `deploy-user-service` with `host=local` runs `systemctl
//! --user` in an operation's cleared environment, which has no
//! `XDG_RUNTIME_DIR`, so every call fails to connect to the user bus.

use super::*;

/// A fake `systemctl` that refuses every call unless `XDG_RUNTIME_DIR` is
/// set, the way a real `systemctl --user` fails to connect to the user
/// bus without it.
const FAKE_SYSTEMCTL_REQUIRES_XDG_RUNTIME_DIR: &str = r#"#!/bin/bash
echo "systemctl $*" >> "$HOME/deploy-calls.log"
if [ -z "$XDG_RUNTIME_DIR" ]; then
  echo "Failed to connect to user scope bus via local transport: No such file or directory" >&2
  exit 1
fi
if [ "$1" = "--user" ] && [ "$2" = "is-active" ]; then
  echo active
fi
exit 0
"#;

/// E3-18: with `host=local` the operation's environment has no
/// `XDG_RUNTIME_DIR`, so every `systemctl --user` call the method makes
/// fails to connect to the user bus and the deploy's check never runs.
#[test]
fn deploy_user_service_sets_xdg_runtime_dir_for_a_local_systemctl_user_bus() {
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "svc", "--purpose", "p", "--repo", repo_s],
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
            "svc",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "deploy-user-service",
            "--arg",
            "host=local",
            "--arg",
            &format!("dest={dest}"),
            "--arg",
            "unit=demo.service",
            "--check",
            "true",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake_rsync(&fakebin.join("rsync"));
    write_fake(
        &fakebin.join("systemctl"),
        FAKE_SYSTEMCTL_REQUIRES_XDG_RUNTIME_DIR,
    );
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
        .args(["deploy", "svc", "prod"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let calls = std::fs::read_to_string(fakehome.join("deploy-calls.log")).unwrap();
    assert!(
        calls.contains("systemctl --user restart demo.service"),
        "{calls}"
    );
    assert!(
        calls.contains("systemctl --user is-active demo.service"),
        "{calls}"
    );

    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["deploy", "log", "svc", "prod", "--json"])
            .stdout,
    )
    .unwrap();
    assert_eq!(rows.as_array().unwrap()[0]["check_ok"], true, "{rows:?}");
}
