use crate::support::*;
use std::os::unix::fs::PermissionsExt;

/// A fake `hcloud`: records every call, reports the firewall as not found
/// (forcing the creation path), and answers `server describe` with a fixed
/// fixture server, already running, at a fixed ipv4 — enough to drive
/// `provision-hetzner.toml`'s firewall, create and wait steps without a
/// real Hetzner account.
const FAKE_HCLOUD: &str = r#"#!/bin/bash
echo "hcloud $*" >> "$HOME/provision-calls.log"
case "$1 $2" in
  "firewall describe")
    exit 1
    ;;
  "firewall create")
    exit 0
    ;;
  "firewall add-rule")
    exit 0
    ;;
  "server create")
    exit 0
    ;;
  "server describe")
    cat <<'JSON'
{"status": "running", "public_net": {"ipv4": {"ip": "203.0.113.9"}}}
JSON
    ;;
  *)
    echo "unexpected hcloud invocation: $*" >&2
    exit 1
    ;;
esac
"#;

fn write_fake(path: &std::path::Path, script: &str) {
    std::fs::write(path, script).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

#[test]
fn provision_creates_a_firewall_and_server_and_records_the_host_suggestion() {
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

    // Provisioning fills in the host of a target already declared: what it
    // is for, and how it deploys, are unrelated to standing up the box.
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "box",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "dest=/srv/app",
            "--check",
            "true",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let cloud_init = e._dir.path().join("cloud-init.yaml");
    std::fs::write(&cloud_init, "#cloud-config\n").unwrap();

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake(&fakebin.join("hcloud"), FAKE_HCLOUD);
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
        .args([
            "provision",
            "demo",
            "box",
            "--arg",
            &format!("cloud_init={}", cloud_init.display()),
            "--arg",
            "ssh_keys=deploy-key,operator-key",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(stdout.contains("203.0.113.9"), "{stdout}");

    // The firewall was checked, created (absent), given all four rules,
    // then the server was created with the defaults, the firewall, the
    // cloud-init file, and both ssh keys.
    let calls = std::fs::read_to_string(fakehome.join("provision-calls.log")).unwrap();
    assert!(calls.contains("hcloud firewall describe box"), "{calls}");
    assert!(
        calls.contains("hcloud firewall create --name box"),
        "{calls}"
    );
    assert!(
        calls.contains("hcloud firewall add-rule box --direction in --protocol tcp --port 22"),
        "{calls}"
    );
    assert!(
        calls.contains("hcloud firewall add-rule box --direction in --protocol tcp --port 80"),
        "{calls}"
    );
    assert!(
        calls.contains("hcloud firewall add-rule box --direction in --protocol tcp --port 443"),
        "{calls}"
    );
    assert!(
        calls.contains("hcloud firewall add-rule box --direction in --protocol icmp"),
        "{calls}"
    );
    assert!(
        calls.contains(&format!(
            "hcloud server create --name box --type cpx21 --location ash --image debian-12 --firewall box --user-data-from-file {}",
            cloud_init.display()
        )),
        "{calls}"
    );
    assert!(calls.contains("--ssh-key deploy-key"), "{calls}");
    assert!(calls.contains("--ssh-key operator-key"), "{calls}");
    assert!(
        calls.contains("hcloud server describe box -o json"),
        "{calls}"
    );

    // The target's host arg now carries the provisioned ipv4; every other
    // field it was declared with is untouched.
    let rows: serde_json::Value = serde_json::from_slice(
        &e.forge("ok.sh", &["project", "deploy", "list", "demo", "--json"])
            .stdout,
    )
    .unwrap();
    let row = &rows.as_array().unwrap()[0];
    assert_eq!(row["args"]["host"], "203.0.113.9", "{row:?}");
    assert_eq!(row["args"]["dest"], "/srv/app", "{row:?}");
    assert_eq!(row["method"], "deploy-command", "{row:?}");

    // An ssh-config fragment landed for the operator to append themselves.
    let ssh_config = e
        .home
        .join("provision")
        .join("demo")
        .join("box")
        .join("ssh-config");
    let fragment = std::fs::read_to_string(&ssh_config).unwrap();
    assert!(fragment.contains("Host box"), "{fragment}");
    assert!(fragment.contains("HostName 203.0.113.9"), "{fragment}");
}

#[test]
fn provision_refuses_an_undeclared_target_and_a_missing_cloud_init_arg() {
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

    // No such target yet.
    let bad = e.forge(
        "ok.sh",
        &[
            "provision",
            "demo",
            "box",
            "--arg",
            "cloud_init=/tmp/nope.yaml",
        ],
    );
    assert!(!bad.status.success());
    let err = String::from_utf8_lossy(&bad.stderr).to_string();
    assert!(
        err.contains("no deploy target box in project demo"),
        "{err}"
    );

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "box",
            "--repo",
            repo_s,
            "--method",
            "deploy-command",
            "--arg",
            "dest=/srv/app",
            "--check",
            "true",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // The target exists now, but no cloud_init arg was given.
    let bad = e.forge("ok.sh", &["provision", "demo", "box"]);
    assert!(!bad.status.success());
    let err = String::from_utf8_lossy(&bad.stderr).to_string();
    assert!(err.contains("cloud_init"), "{err}");
}
