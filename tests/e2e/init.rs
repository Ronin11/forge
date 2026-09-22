//! `forge init`: see doctor.rs and init.rs. No systemd session is reachable
//! in this suite's sandbox (no `/run/systemd/system`, no
//! `XDG_RUNTIME_DIR`), and `XDG_CONFIG_HOME` is pointed at the test's own
//! tempdir (`Env::xdg_config`), so every assertion here exercises the
//! print path and never touches the machine running the suite.

use crate::support::*;

#[test]
fn forge_init_sets_up_a_fresh_home_and_prints_the_systemd_commands() {
    let e = Env::new();
    assert!(!e.home.exists());

    let o = e.forge("ok.sh", &["init"]);
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
    assert!(e.forge("ok.sh", &["init"]).status.success());
    let catalog = e.home.join("workflows");
    let head_after_first = git(&catalog, &["rev-parse", "HEAD"]);
    let worker_unit_path = e.xdg_config.join("systemd/user/forge-worker.service");
    let unit_after_first = std::fs::read_to_string(&worker_unit_path).unwrap();

    let o = e.forge("ok.sh", &["init"]);
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
    let o = e
        .cmd("ok.sh")
        .args(["init", "--home", custom.to_str().unwrap()])
        .output()
        .expect("forge init --home");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(custom.join("config.toml").exists());
    assert!(!e.home.exists(), "the default home must not be touched");
}
