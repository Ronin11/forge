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

// A just-copied executable can transiently fail to spawn with ETXTBSY under
// concurrent process load. Retry only that launch error; no child ran yet.
fn copied_binary_output(cmd: &mut std::process::Command) -> std::io::Result<Output> {
    for attempt in 0..5 {
        match cmd.output() {
            Err(err) if err.raw_os_error() == Some(libc::ETXTBSY) && attempt < 4 => {
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
            result => return result,
        }
    }
    unreachable!()
}

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
    assert!(
        !catalog.join("actions/code.toml").exists(),
        "built-in actions are not seeded into the catalog"
    );
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
    assert!(worker_unit.contains(&format!("Environment=\"FORGE_HOME={}\"", e.home.display())));

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

/// Answers `is-enabled` from `INIT_ENABLED` (`yes` = enabled, otherwise
/// not), `show` from `INIT_SHOW`, and succeeds at everything else. Rejects
/// a path argument to `is-enabled` (a `/` in `$3`) the way a real systemd
/// does ("Invalid argument"), so a regression back to asking by unit file
/// path fails these tests instead of hiding behind a lenient fake.
const FAKE_SYSTEMCTL: &str = r#"#!/bin/bash
echo "systemctl $*" >> "$INIT_CALLS_LOG"
case "$2" in
  is-enabled)
    case "$3" in
      */*) echo "Invalid argument" >&2; exit 1 ;;
    esac
    [ "$INIT_ENABLED" = yes ] && exit 0 || exit 1 ;;
  show) printf '%s\n' "$INIT_SHOW" ;;
esac
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
            "systemctl --user show forge-worker.service -p MainPID -p Environment",
            "systemctl --user daemon-reload",
            "systemctl --user is-enabled forge-worker.service",
            "systemctl --user is-enabled forge-web.service",
            &enable,
            "loginctl enable-linger"
        ],
        "{calls:?}"
    );
}

#[test]
fn forge_init_relink_moves_an_existing_install_onto_the_release_layout() {
    let e = Env::new();
    // The old layout: ~/.local/bin/forge -> a build directory's binary.
    let home_dir = e._dir.path().join("userhome");
    let build = e._dir.path().join("target-release");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_forge"), build.join("forge")).unwrap();
    for b in ["forge-web", "forge-portal", "forge-repomap", "forge-tui"] {
        write_fake(&build.join(b), "#!/bin/sh\nexit 0\n");
    }
    let local_bin = home_dir.join(".local/bin");
    std::fs::create_dir_all(&local_bin).unwrap();
    std::os::unix::fs::symlink(build.join("forge"), local_bin.join("forge")).unwrap();

    let relink = || {
        // Run through the old symlink, as the operator's shell would.
        let o = copied_binary_output(
            std::process::Command::new(local_bin.join("forge"))
                .env("FORGE_HOME", &e.home)
                .env("XDG_CONFIG_HOME", &e.xdg_config)
                .env("HOME", &home_dir)
                .env("FORGE_SUPERVISOR", "0")
                .env_remove("XDG_RUNTIME_DIR")
                .env_remove("DBUS_SESSION_BUS_ADDRESS")
                .args(["init", "--relink"]),
        )
        .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
    };

    let out = relink();
    let bin = e.home.join("bin");
    let target = std::fs::read_link(bin.join("current")).unwrap();
    let id = target.file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        bin.join("releases").join(&id).join("forge").is_file(),
        "{out}"
    );
    assert!(
        bin.join("releases").join(&id).join("forge-web").is_file(),
        "{out}"
    );
    assert_eq!(
        std::fs::read_link(local_bin.join("forge")).unwrap(),
        bin.join("current/forge")
    );
    let unit =
        std::fs::read_to_string(e.xdg_config.join("systemd/user/forge-worker.service")).unwrap();
    assert!(
        unit.contains(&format!(
            "ExecStart=\"{}/current/forge\" \"work\"",
            bin.display()
        )),
        "{unit}"
    );
    assert!(out.contains("done release"), "{out}");
    // The id is the full commit hash, the name `deploy-self` gives it.
    // (A build with no git at all falls back to the crate version.)
    if id.chars().all(|c| c.is_ascii_hexdigit()) {
        assert_eq!(id.len(), 40, "{id}");
    }

    let out = relink();
    assert!(
        out.contains("already initialized; nothing changed"),
        "{out}"
    );
    assert_eq!(std::fs::read_link(bin.join("current")).unwrap(), target);
}

#[test]
fn forge_init_relink_refuses_a_directory_with_only_forge() {
    let e = Env::new();
    let home_dir = e._dir.path().join("userhome");
    let build = e._dir.path().join("target-debug");
    std::fs::create_dir_all(&build).unwrap();
    std::fs::create_dir_all(&home_dir).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_forge"), build.join("forge")).unwrap();

    let o = copied_binary_output(
        std::process::Command::new(build.join("forge"))
            .env("FORGE_HOME", &e.home)
            .env("XDG_CONFIG_HOME", &e.xdg_config)
            .env("HOME", &home_dir)
            .env("FORGE_SUPERVISOR", "0")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .args(["init", "--relink"]),
    )
    .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{out}");
    for b in ["forge-web", "forge-portal", "forge-repomap", "forge-tui"] {
        assert!(out.contains(b), "{b} not named: {out}");
    }
    assert!(!out.contains("forge-test"), "{out}");
    let bin = e.home.join("bin");
    assert!(std::fs::symlink_metadata(bin.join("current")).is_err());
    let releases: Vec<_> = std::fs::read_dir(bin.join("releases"))
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(releases.is_empty(), "{releases:?}");
}

#[test]
fn forge_demo_fake_lands_a_task_and_a_second_run_offers_reset() {
    let e = Env::new();
    let o = e.forge("ok.sh", &["demo", "--fake"]);
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{out}");
    assert!(out.contains("forge trace 1"), "{out}");
    assert!(out.contains("landed"), "{out}");
    let origin = e.home.join("demo/origin.git");
    assert_eq!(git(&origin, &["show", "main:answer.txt"]), "42");

    let o = e.forge("ok.sh", &["demo", "--fake"]);
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success() && out.contains("--reset"), "{out}");

    let o = e.forge("ok.sh", &["demo", "--fake", "--reset"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
}

#[test]
fn forge_demo_fake_overrides_a_configured_codex_provider_and_roles() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[providers.cx]\nrunner = \"codex-cli\"\n[roles]\ncode = \"cx\"\nreview = \"cx\"\nplan = \"cx\"\ntests = \"cx\"\n",
    )
    .unwrap();
    let mut cmd = e.cmd("ok.sh");
    cmd.env("FORGE_CODEX_BIN", "/nonexistent/codex")
        .env("FORGE_CLAUDE_BIN_CODE", "/nonexistent/role-claude")
        .args(["demo", "--fake"]);
    let o = cmd.output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{out}");
    let origin = e.home.join("demo/origin.git");
    assert_eq!(git(&origin, &["show", "main:answer.txt"]), "42");
}

#[test]
fn forge_init_bakes_the_shell_path_into_the_units_and_prints_it() {
    let e = Env::new();
    let agents = e.home.parent().unwrap().join("agent-cli-dir");
    std::fs::create_dir_all(&agents).unwrap();
    write_fake(&agents.join("claude"), "#!/bin/sh\nexit 0\n");
    let shell_path = format!("{}:/usr/bin:/bin", agents.display());
    let mut cmd = e.cmd("ok.sh");
    cmd.env_remove("XDG_RUNTIME_DIR")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env("PATH", &shell_path)
        .arg("init");
    let o = cmd.output().expect("forge init");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    let unit_dir = e.xdg_config.join("systemd/user");
    for name in ["forge-worker.service", "forge-web.service"] {
        let unit = std::fs::read_to_string(unit_dir.join(name)).unwrap();
        let line = unit
            .lines()
            .find(|l| l.starts_with("Environment=\"PATH="))
            .unwrap();
        let value = line["Environment=\"PATH=".len()..]
            .strip_suffix('"')
            .unwrap();
        let dirs: Vec<&str> = value.split(':').collect();
        assert_eq!(
            &dirs[1..],
            [agents.to_str().unwrap(), "/usr/bin", "/bin"],
            "{line}"
        );
    }
    let worker = std::fs::read_to_string(unit_dir.join("forge-worker.service")).unwrap();
    assert!(worker.contains("StartLimitBurst="), "{worker}");
    assert!(
        out.contains(&agents.display().to_string()),
        "PATH is printed: {out}"
    );
}

#[test]
fn doctor_fails_when_the_worker_units_path_cannot_find_claude() {
    let e = Env::new();
    assert!(e.run("ok.sh", &[]).status.success());
    let agents = e.home.parent().unwrap().join("agent-cli-dir");
    std::fs::create_dir_all(&agents).unwrap();
    write_fake(&agents.join("claude"), "#!/bin/sh\nexit 0\n");
    let unit_dir = e.xdg_config.join("systemd/user");
    std::fs::create_dir_all(&unit_dir).unwrap();
    std::fs::write(
        unit_dir.join("forge-worker.service"),
        "[Service]\nEnvironment=PATH=/usr/local/bin:/usr/bin:/bin\n",
    )
    .unwrap();
    let mut cmd = e.cmd("ok.sh");
    cmd.env("FORGE_CLAUDE_BIN", "claude")
        .env(
            "PATH",
            format!("{}:/usr/local/bin:/usr/bin:/bin", agents.display()),
        )
        .arg("doctor");
    let o = cmd.output().expect("forge doctor");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{out}");
    let row = out
        .lines()
        .find(|l| l.contains("binary.claude"))
        .expect(&out);
    assert!(
        row.contains("FAIL") && row.contains("forge-worker.service"),
        "{out}"
    );
    assert!(
        out.contains("forge init --relink"),
        "the fix is named: {out}"
    );
}

#[test]
fn forge_init_units_survive_a_home_and_path_with_spaces_under_systemd_analyze_verify() {
    let probe = std::process::Command::new("systemd-analyze")
        .arg("--version")
        .output();
    if !probe.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("systemd-analyze not available; skipping");
        return;
    }
    let e = Env::new();
    let home = e.home.parent().unwrap().join("my home");
    let agents = e.home.parent().unwrap().join("agent dir$x");
    std::fs::create_dir_all(&agents).unwrap();
    let shell_path = format!("{}:/usr/bin:/bin", agents.display());
    let mut cmd = e.cmd("ok.sh");
    cmd.env_remove("XDG_RUNTIME_DIR")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env("PATH", &shell_path)
        .arg("init")
        .arg("--home")
        .arg(&home);
    let o = cmd.output().expect("forge init");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let unit_dir = e.xdg_config.join("systemd/user");
    for name in ["forge-worker.service", "forge-web.service"] {
        let file = unit_dir.join(name);
        let unit = std::fs::read_to_string(&file).unwrap();
        assert!(
            unit.contains(&format!("Environment=\"FORGE_HOME={}\"", home.display())),
            "{unit}"
        );
        assert!(unit.contains("dir$x") && !unit.contains("dir$$x"), "{unit}");
        let v = std::process::Command::new("systemd-analyze")
            .args(["--user", "verify"])
            .arg(&file)
            .output()
            .expect("systemd-analyze verify");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&v.stdout),
            String::from_utf8_lossy(&v.stderr)
        );
        // A missing forge-web next to the test binary is not what is under test.
        let bad: Vec<&str> = text
            .lines()
            .filter(|l| !l.contains("is not executable") && !l.contains("No such file"))
            .filter(|l| l.contains(name))
            .collect();
        assert!(bad.is_empty(), "{name}: {text}");
    }
}

/// REVIEW-4 E3-17: a checked-in `deploy/forge-worker.service` had drifted
/// from the unit `forge init` actually writes (`Type=simple`, no
/// `NotifyAccess=all`, no `StartLimit*`), so `MAINPID=` from a successor
/// was ignored and the old worker's exit after a handover killed it, while
/// `docs/DEPLOY.md` cited the stale file for the drain semantics.
/// `init::worker_unit` is now the only source: this fails if the file
/// comes back, and pins the fields the successor handoff depends on.
#[test]
fn forge_init_is_the_only_source_of_the_worker_unit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("deploy/forge-worker.service").exists(),
        "deploy/forge-worker.service must stay deleted: `forge init` \
         (init::worker_unit) is the only source of the worker unit"
    );

    let e = Env::new();
    let o = forge_init_no_session(&e, &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let unit =
        std::fs::read_to_string(e.xdg_config.join("systemd/user/forge-worker.service")).unwrap();
    assert!(unit.contains("Type=notify"), "{unit}");
    assert!(unit.contains("NotifyAccess=all"), "{unit}");
    assert!(unit.contains("StartLimitIntervalSec="), "{unit}");
    assert!(unit.contains("StartLimitBurst="), "{unit}");
}

/// A reachable fake session: returns the `init` command, the calls log and
/// the unit directory. `enabled` is what the fake `is-enabled` answers;
/// `show` is what its `show` prints.
struct Session {
    calls_log: std::path::PathBuf,
    unit_dir: std::path::PathBuf,
    run_dir: std::path::PathBuf,
    runtime_dir: std::path::PathBuf,
    fakebin: std::path::PathBuf,
}

impl Session {
    fn new(e: &Env) -> Session {
        let root = e._dir.path();
        let s = Session {
            calls_log: root.join("init-calls.log"),
            unit_dir: e.xdg_config.join("systemd/user"),
            run_dir: root.join("run"),
            runtime_dir: root.join("runtime"),
            fakebin: root.join("fakebin"),
        };
        std::fs::create_dir_all(s.run_dir.join("systemd/system")).unwrap();
        std::fs::create_dir_all(&s.runtime_dir).unwrap();
        std::fs::create_dir_all(&s.fakebin).unwrap();
        write_fake(&s.fakebin.join("systemctl"), FAKE_SYSTEMCTL);
        write_fake(&s.fakebin.join("loginctl"), FAKE_LOGINCTL);
        s
    }

    /// `forge init` with `shell_path` after the fakes; returns stdout.
    fn init(&self, e: &Env, enabled: &str, show: &str, shell_path: &str, extra: &[&str]) -> String {
        let _ = std::fs::remove_file(&self.calls_log);
        let o = e
            .cmd("ok.sh")
            .env("PATH", format!("{}:{shell_path}", self.fakebin.display()))
            .env("FORGE_TEST_SYSTEMD_RUN_DIR", &self.run_dir)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("INIT_CALLS_LOG", &self.calls_log)
            .env("INIT_ENABLED", enabled)
            .env("INIT_SHOW", show)
            .arg("init")
            .args(extra)
            .output()
            .expect("forge init");
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(o.status.success(), "{out}");
        out
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.calls_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The PATH a unit's `Environment="PATH=..."` line declares, unquoted.
    fn unit_path(&self, unit: &str) -> String {
        let text = std::fs::read_to_string(self.unit_dir.join(unit)).unwrap();
        let line = text
            .lines()
            .find(|l| l.starts_with("Environment=\"PATH="))
            .unwrap();
        line["Environment=\"PATH=".len()..]
            .strip_suffix('"')
            .unwrap()
            .to_string()
    }
}

#[test]
fn forge_init_asks_systemd_and_enables_only_what_is_not_enabled() {
    let e = Env::new();
    let s = Session::new(&e);
    let shell = "/usr/bin:/bin";
    // The first run had no session: files exist, nothing is enabled.
    s.init(&e, "no", "", shell, &[]);
    let worker = s.unit_dir.join("forge-worker.service");
    let web = s.unit_dir.join("forge-web.service");

    // Unchanged files, but systemd says neither is enabled: enable both,
    // never "already installed and enabled".
    let out = s.init(&e, "no", "", shell, &[]);
    assert!(!out.contains("already installed and enabled"), "{out}");
    let enable = format!(
        "systemctl --user enable --now {} {}",
        worker.display(),
        web.display()
    );
    assert!(s.calls().contains(&enable), "{:?}", s.calls());
    assert!(s.calls().contains(&"loginctl enable-linger".to_string()));
    assert!(!s.calls().iter().any(|c| c.contains("daemon-reload")));

    // Unchanged and enabled: asked, nothing enabled, reported as such.
    let out = s.init(&e, "yes", "", shell, &[]);
    assert!(out.contains("already installed and enabled"), "{out}");
    assert!(
        out.contains("already initialized; nothing changed"),
        "{out}"
    );
    let calls = s.calls();
    assert_eq!(
        calls,
        [
            "systemctl --user is-enabled forge-worker.service",
            "systemctl --user is-enabled forge-web.service",
        ]
    );
}

#[test]
fn forge_init_says_a_changed_unit_needs_a_restart_and_names_the_live_path() {
    let e = Env::new();
    let s = Session::new(&e);
    s.init(&e, "yes", "", "/usr/bin:/bin", &[]);

    // The worker is running with an older PATH; this run changes the unit.
    let show = "MainPID=0\nEnvironment=FORGE_HOME=/h PATH=/old/live:/usr/bin";
    let out = s.init(&e, "yes", show, "/opt/new:/usr/bin:/bin", &[]);
    assert!(out.contains("restart forge-worker to apply"), "{out}");
    assert!(
        out.contains("the running worker's PATH is /old/live:/usr/bin"),
        "{out}"
    );
    assert!(!out.contains("installed and enabled"), "{out}");
    assert!(
        s.calls().iter().all(|c| !c.contains("enable --now")),
        "{:?}",
        s.calls()
    );

    // No change, no restart advice.
    let out = s.init(&e, "yes", show, "/opt/new:/usr/bin:/bin", &[]);
    assert!(!out.contains("restart forge-worker"), "{out}");
}

#[test]
fn forge_init_keeps_the_units_existing_path_entries_unless_reset_path() {
    let e = Env::new();
    let s = Session::new(&e);
    s.init(&e, "yes", "", "/opt/agents:/usr/bin:/bin", &[]);
    for unit in ["forge-worker.service", "forge-web.service"] {
        assert!(s.unit_path(unit).contains("/opt/agents"));
    }

    // A narrower shell: the agents directory stays, after the new entries.
    s.init(&e, "yes", "", "/usr/bin:/bin", &[]);
    for unit in ["forge-worker.service", "forge-web.service"] {
        let value = s.unit_path(unit);
        let dirs: Vec<&str> = value.split(':').collect();
        assert_eq!(
            &dirs[dirs.len() - 3..],
            ["/usr/bin", "/bin", "/opt/agents"],
            "{value}"
        );
        assert!(
            dirs.iter().position(|d| *d == "/usr/bin")
                < dirs.iter().position(|d| *d == "/opt/agents"),
            "{value}"
        );
    }

    // Stable on a re-run from the same narrow shell.
    let before = s.unit_path("forge-worker.service");
    s.init(&e, "yes", "", "/usr/bin:/bin", &[]);
    assert_eq!(s.unit_path("forge-worker.service"), before);

    // --reset-path writes the shell's PATH alone.
    s.init(&e, "yes", "", "/usr/bin:/bin", &["--reset-path"]);
    for unit in ["forge-worker.service", "forge-web.service"] {
        let value = s.unit_path(unit);
        assert!(!value.contains("/opt/agents"), "{value}");
    }
}
