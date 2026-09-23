//! `forge init`: see doctor.rs and init.rs. `XDG_CONFIG_HOME` is pointed at
//! the test's own tempdir (`Env::xdg_config`) in every test here, so the
//! unit files `forge init` writes always land under a tempdir rather than
//! the machine running the suite's real `~/.config/systemd/user`.
//!
//! The no-session test below additionally strips `XDG_RUNTIME_DIR` and
//! `DBUS_SESSION_BUS_ADDRESS` from the child's environment, so it exercises
//! the print path even when `cargo test` itself runs inside a logged-in
//! desktop session (where both are set) rather than accidentally touching
//! that session's real `systemctl`/`loginctl`.
//!
//! `forge_init_enables_units_when_a_systemd_session_is_reachable` below
//! covers the other branch: a fake `systemctl`/`loginctl` first on `PATH`,
//! and `FORGE_TEST_SYSTEMD_RUN_DIR` pointed at a tempdir standing in for
//! `/run` (the one piece `sd_booted()` checks that a test can't otherwise
//! fake, since it is only ever real once systemd is actually pid 1).

use crate::support::*;
use std::path::Path;
use std::process::Output;

fn write_fake(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

/// `forge init`, with `XDG_RUNTIME_DIR` and `DBUS_SESSION_BUS_ADDRESS`
/// stripped from the child's environment so a developer's own logged-in
/// session (where both are set) never makes these tests take the
/// systemd-session branch and touch that session's real `systemctl` /
/// `loginctl`.
fn forge_init_no_session(e: &Env, extra: &[&str]) -> Output {
    let mut cmd = e.cmd("ok.sh");
    cmd.env_remove("XDG_RUNTIME_DIR")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .arg("init")
        .args(extra);
    let o = cmd.output().expect("forge init");
    eprintln!(
        "--- forge init {} ---\n{}{}",
        extra.join(" "),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

#[test]
fn forge_init_sets_up_a_fresh_home_and_prints_the_systemd_commands() {
    let e = Env::new();
    assert!(!e.home.exists());

    let o = forge_init_no_session(&e, &[]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");

    assert!(e.home.is_dir());
    assert!(e.home.join("config.toml").exists());
    let token = std::fs::read_to_string(e.home.join("web.token")).unwrap();
    assert!(token.trim().len() >= 32);

    let catalog = e.home.join("workflows");
    assert!(catalog.join(".git").exists());
    assert!(catalog.join("actions/code.toml").exists());
    // Everything the catalog holds after init is committed: `git status`
    // is clean, matching workflows::uncommitted's own porcelain check.
    let status = git(&catalog, &["status", "--porcelain"]);
    assert!(status.is_empty(), "catalog has uncommitted files: {status}");
    let log = git(&catalog, &["log", "--oneline"]);
    assert_eq!(log.lines().count(), 1, "one commit: {log}");

    let unit_dir = e.xdg_config.join("systemd/user");
    assert!(unit_dir.join("forge-worker.service").exists());
    assert!(unit_dir.join("forge-web.service").exists());
    let worker_unit = std::fs::read_to_string(unit_dir.join("forge-worker.service")).unwrap();
    assert!(worker_unit.contains("ExecStart="));
    assert!(worker_unit.contains(&format!("FORGE_HOME={}", e.home.display())));

    assert!(out.contains("no systemd user session detected"), "{out}");
    assert!(out.contains("systemctl --user daemon-reload"), "{out}");
    assert!(out.contains("loginctl enable-linger"), "{out}");

    // The closing doctor pass, against the same home just set up.
    assert!(out.contains("binary.git"), "{out}");
    assert!(out.contains("sandbox"), "{out}");
}

#[test]
fn forge_init_run_twice_changes_nothing_the_second_time() {
    let e = Env::new();
    assert!(forge_init_no_session(&e, &[]).status.success());
    let catalog = e.home.join("workflows");
    let head_after_first = git(&catalog, &["rev-parse", "HEAD"]);
    let worker_unit_path = e.xdg_config.join("systemd/user/forge-worker.service");
    let unit_after_first = std::fs::read_to_string(&worker_unit_path).unwrap();

    let o = forge_init_no_session(&e, &[]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("already initialized; nothing changed"),
        "{out}"
    );
    for name in ["home", "config", "workflows", "web.token", "systemd"] {
        assert!(
            out.lines()
                .any(|l| l.starts_with("ok  ") && l.contains(name)),
            "expected {name} unchanged on the second run:\n{out}"
        );
    }

    assert_eq!(git(&catalog, &["rev-parse", "HEAD"]), head_after_first);
    assert_eq!(
        std::fs::read_to_string(&worker_unit_path).unwrap(),
        unit_after_first
    );
}

#[test]
fn forge_init_home_overrides_the_default_resolution() {
    let e = Env::new();
    let custom = e._dir.path().join("elsewhere");
    let o = forge_init_no_session(&e, &["--home", custom.to_str().unwrap()]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(custom.join("config.toml").exists());
    assert!(!e.home.exists(), "the default home must not be touched");
}

const FAKE_SYSTEMCTL: &str = r#"#!/bin/bash
echo "systemctl $*" >> "$INIT_CALLS_LOG"
exit 0
"#;

const FAKE_LOGINCTL: &str = r#"#!/bin/bash
echo "loginctl $*" >> "$INIT_CALLS_LOG"
exit 0
"#;

/// The session branch: `/run/systemd/system` (faked via
/// `FORGE_TEST_SYSTEMD_RUN_DIR`) and `XDG_RUNTIME_DIR` both present, and a
/// fake `systemctl`/`loginctl` first on `PATH` recording their exact argv
/// so nothing real is ever reachable — `forge init` can only invoke the
/// fakes, and the assertions below pin down that it invokes exactly the
/// three commands `install_units` is meant to run, on the unit file paths
/// it just wrote under this test's own `XDG_CONFIG_HOME`.
#[test]
fn forge_init_enables_units_when_a_systemd_session_is_reachable() {
    let e = Env::new();

    let run_dir = e._dir.path().join("run");
    std::fs::create_dir_all(run_dir.join("systemd/system")).unwrap();
    let runtime_dir = e._dir.path().join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();

    let fakebin = e._dir.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    write_fake(&fakebin.join("systemctl"), FAKE_SYSTEMCTL);
    write_fake(&fakebin.join("loginctl"), FAKE_LOGINCTL);
    let path = format!(
        "{}:{}",
        fakebin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let calls_log = e._dir.path().join("init-calls.log");

    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .env("FORGE_TEST_SYSTEMD_RUN_DIR", &run_dir)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("INIT_CALLS_LOG", &calls_log)
        .args(["init"])
        .output()
        .expect("forge init");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");

    let unit_dir = e.xdg_config.join("systemd/user");
    let worker_path = unit_dir.join("forge-worker.service");
    let web_path = unit_dir.join("forge-web.service");
    assert!(worker_path.exists());
    assert!(web_path.exists());

    assert!(out.contains("installed and enabled"), "{out}");
    assert!(!out.contains("no systemd user session detected"), "{out}");

    let calls = std::fs::read_to_string(&calls_log).unwrap_or_default();
    let calls: Vec<&str> = calls.lines().collect();
    let enable = format!(
        "systemctl --user enable --now {} {}",
        worker_path.display(),
        web_path.display()
    );
    assert_eq!(
        calls,
        [
            "systemctl --user daemon-reload",
            &enable,
            "loginctl enable-linger"
        ],
        "{calls:?}"
    );
}
