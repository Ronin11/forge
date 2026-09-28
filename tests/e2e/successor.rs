//! The successor worker (docs/OPS.md, "The running binary"): a release
//! staged under a running worker starts a successor on it that claims the
//! next task while the old worker finishes the one it holds and exits, and
//! doctor says so. The releases are copies of this suite's own `forge`.

use crate::deploy::SelfDeploy;
use crate::support::*;
use std::time::Duration;

/// Kills the successor, which runs in its own process group, whatever
/// happens to the test.
struct Reap(std::path::PathBuf);

impl Drop for Reap {
    fn drop(&mut self) {
        let Ok(c) = rusqlite::Connection::open(self.0.join("forge.db")) else {
            return;
        };
        let Ok(mut s) = c.prepare("SELECT pid FROM workers") else {
            return;
        };
        let pids: Vec<i64> = s
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        for pid in pids {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
}

fn running_pid(e: &Env, id: i64) -> Option<i64> {
    e.db()
        .query_row(
            "SELECT worker_pid FROM tasks WHERE id=?1 AND state='running'",
            [id],
            |r| r.get(0),
        )
        .ok()
        .flatten()
}

#[test]
fn a_staged_release_starts_a_successor_that_claims_while_the_old_worker_drains() {
    let e = Env::new();
    let root = e.home.join("bin");
    let fakes = e.home.join("fakebin");
    std::fs::create_dir_all(&fakes).unwrap();
    let calls = e.home.join("calls.log");
    let systemctl = fakes.join("systemctl");
    std::fs::write(
        &systemctl,
        "#!/bin/bash\necho \"systemctl $*\" >> \"$SUCC_CALLS_LOG\"\nif [ \"$2\" = is-active ]; then echo active; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(
        &systemctl,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    for id in ["old", "new"] {
        let dir = root.join("releases").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let built = std::path::Path::new(env!("CARGO_BIN_EXE_forge"))
            .parent()
            .unwrap();
        for bin in ["forge", "forge-repomap"] {
            std::fs::copy(built.join(bin), dir.join(bin)).unwrap();
        }
    }
    std::os::unix::fs::symlink("releases/old", root.join("current")).unwrap();
    let path = format!(
        "{}:{}",
        fakes.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let first = e.add(&["--retries", "0"]);
    // The old release's copy of `forge`, in `cmd`'s environment.
    let mut cmd = std::process::Command::new(root.join("releases/old/forge"));
    cmd.envs(
        e.cmd("slow-ok.sh")
            .get_envs()
            .filter_map(|(k, v)| Some((k, v?))),
    )
    .env("PATH", &path)
    .env("SUCC_CALLS_LOG", &calls)
    .args(["work", "--poll", "1"]);
    let mut old = Worker::spawn(&mut cmd);
    let _reap = Reap(e.home.clone());
    assert!(
        wait_until(|| running_pid(&e, first).is_some(), Duration::from_secs(30)),
        "the old worker never claimed task {first}"
    );
    let old_pid = running_pid(&e, first).unwrap();
    assert_eq!(old_pid, i64::from(old.id()));

    std::os::unix::fs::symlink("releases/new", root.join("staged")).unwrap();
    let second = e.add(&["--retries", "0"]);
    assert!(
        wait_until(
            || running_pid(&e, second).is_some(),
            Duration::from_secs(30)
        ),
        "the successor never claimed task {second}"
    );
    let new_pid = running_pid(&e, second).unwrap();
    assert_ne!(new_pid, old_pid, "two workers claimed as one");
    assert_eq!(
        e.task(first).0,
        "running",
        "the old worker's attempt was cut short"
    );
    let version: String = e
        .db()
        .query_row("SELECT version FROM workers WHERE pid=?1", [new_pid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(version, "new");

    let out = String::from_utf8_lossy(&e.forge("ok.sh", &["doctor"]).stdout).to_string();
    let row = out.lines().find(|l| l.contains("worker")).unwrap_or("");
    assert!(
        row.contains("release new staged; successor pid")
            && row.contains(&format!("{new_pid} claiming"))
            && row.contains("1 attempts draining on old"),
        "{out}"
    );

    // The old worker finishes the attempt it holds and exits by itself.
    assert!(
        old.wait().success(),
        "the old worker did not exit cleanly after draining"
    );
    assert_ne!(e.task(first).0, "running");
    assert_eq!(
        std::fs::read_link(root.join("current")).unwrap(),
        std::path::Path::new("releases/new")
    );
    let calls = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        calls.contains("systemctl --user restart --no-block forge-web")
            && !calls.contains("forge-portal"),
        "{calls}"
    );
    assert!(
        wait_until(|| e.task(second).0 != "running", Duration::from_secs(60)),
        "task {second} never finished"
    );
    let claimed: i64 = e
        .db()
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id IN (?1, ?2) AND state IN ('succeeded', 'failed')",
            [first, second],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(claimed, 2);
}

#[test]
fn a_successor_exiting_without_taking_over_makes_the_old_worker_fail() {
    let e = Env::new();
    let root = e.home.join("bin");
    let release = root.join("releases/new");
    std::fs::create_dir_all(&release).unwrap();
    let bin = release.join("forge");
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("releases/new", root.join("staged")).unwrap();

    let output = e
        .cmd("ok.sh")
        .env("FORGE_RELEASE", "old")
        .env("NOTIFY_SOCKET", e.home.join("missing.sock"))
        .env_remove("FORGE_SUCCESSOR_OF")
        .args(["work", "--poll", "1"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("successor pid "), "{stderr}");
    assert!(stderr.contains("for release new exited ("), "{stderr}");
    assert!(stderr.contains("without taking the unit over"), "{stderr}");
    assert!(stderr.contains("worked 0 task(s)"), "{stderr}");
    let stopped: bool = e
        .db()
        .query_row(
            "SELECT stopped_at IS NOT NULL FROM workers WHERE version='old'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(stopped, "the old worker must deregister even on failure");
}

/// A fake `systemctl` whose `is-active` answers what `$SUCC_UNIT_STATE`
/// (a file) says, and which records every call.
fn fake_systemctl(e: &Env) -> (String, std::path::PathBuf) {
    let fakes = e.home.join("fakebin");
    std::fs::create_dir_all(&fakes).unwrap();
    let state = e.home.join("unit-state");
    std::fs::write(&state, "active\n").unwrap();
    let systemctl = fakes.join("systemctl");
    std::fs::write(
        &systemctl,
        format!(
            "#!/bin/bash\necho \"systemctl $*\" >> \"{}\"\nif [ \"$2\" = is-active ]; then cat \"{}\"; fi\nexit 0\n",
            e.home.join("calls.log").display(),
            state.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &systemctl,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        fakes.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (path, state)
}

#[test]
fn a_successor_takes_the_unit_over_and_honours_a_stop_job_that_arrives_while_it_claims() {
    let e = Env::new();
    let (path, state) = fake_systemctl(&e);
    let sock_path = e.home.join("notify.sock");
    let sock = std::os::unix::net::UnixDatagram::bind(&sock_path).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();

    let mut cmd = e.cmd("slow-ok.sh");
    cmd.env("PATH", &path)
        .env("NOTIFY_SOCKET", &sock_path)
        .env("FORGE_SUCCESSOR_OF", "1")
        .args(["work", "--poll", "1"]);
    let mut w = Worker::spawn(&mut cmd);
    let _reap = Reap(e.home.clone());

    // It tells systemd it is the unit's main pid and ready, and only then
    // names itself in the capability file the old worker waits on.
    let mut buf = [0u8; 256];
    let n = sock.recv(&mut buf).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&buf[..n]),
        format!("MAINPID={}\nREADY=1", w.id())
    );
    assert!(
        wait_until(
            || std::fs::read_to_string(e.home.join("bin/successor-capable"))
                .is_ok_and(|s| s.trim() == w.id().to_string()),
            Duration::from_secs(30)
        ),
        "the successor never wrote the capability file"
    );

    let first = e.add(&["--retries", "0"]);
    assert!(
        wait_until(|| running_pid(&e, first).is_some(), Duration::from_secs(30)),
        "the successor never claimed task {first}"
    );

    // An operator's restart: the unit is deactivating, and systemd's
    // SIGTERM went to a pid that is gone. The successor drains and exits.
    std::fs::write(&state, "deactivating\n").unwrap();
    let second = e.add(&["--retries", "0"]);
    assert!(w.wait().success(), "the successor did not exit cleanly");
    assert_ne!(e.task(first).0, "running");
    assert_ne!(e.task(first).0, "queued");
    assert_eq!(e.task(second).0, "queued", "claimed under a stop job");
}

#[test]
fn doctor_fails_the_worker_row_when_the_unit_is_deactivating_with_a_claiming_worker() {
    let e = Env::new();
    let (path, state) = fake_systemctl(&e);
    assert!(e.forge("ok.sh", &["doctor"]).status.code().is_some());
    e.db()
        .execute(
            "INSERT INTO workers (pid, version, started_at) VALUES (?1, 'new', 0)",
            [i64::from(std::process::id())],
        )
        .unwrap();
    std::fs::write(&state, "deactivating\n").unwrap();
    let o = e
        .cmd("ok.sh")
        .env("PATH", &path)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let checks: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let row = checks
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "worker")
        .unwrap();
    assert_eq!(row["status"], "fail", "{row}");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("unit forge-worker deactivating with a claiming worker"),
        "{row}"
    );
    assert!(
        row["hint"]
            .as_str()
            .unwrap()
            .contains(&format!("kill -TERM {}", std::process::id())),
        "{row}"
    );
}

/// The worker-unit restarts in a self-deploy's recorded calls.
fn worker_restarts(calls: &[String]) -> usize {
    calls
        .iter()
        .filter(|c| {
            c.starts_with("systemctl") && c.contains("restart") && c.contains("forge-worker")
        })
        .count()
}

#[test]
fn deploy_self_only_stages_for_a_successor_capable_worker_and_restarts_an_older_one() {
    // Not capable: no worker registered, no capability file. One restart.
    let legacy = SelfDeploy::new();
    let sha = legacy.commit("good");
    assert!(legacy.deploy(&sha).status.success());
    assert_eq!(legacy.link("staged"), format!("releases/{sha}"));
    assert_eq!(worker_restarts(&legacy.calls()), 1, "{:?}", legacy.calls());

    // Capable by the workers table: a live worker (this test's own pid).
    let s = SelfDeploy::new();
    s.e.db()
        .execute(
            "INSERT INTO workers (pid, version, started_at) VALUES (?1, 'old', 0)",
            [i64::from(std::process::id())],
        )
        .unwrap();
    let sha = s.commit("good");
    let o = s.deploy_taken_over(&sha);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(s.link("staged"), format!("releases/{sha}"));
    // The successor flips current; the deploy leaves it alone.
    assert_eq!(s.link("current"), format!("releases/{sha}"));
    let calls = s.calls();
    assert_eq!(worker_restarts(&calls), 0, "{calls:?}");
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("systemctl") && c.contains("restart")),
        "{calls:?}"
    );
    assert_eq!(s.deploy_rows()[0]["check_ok"], true);

    // Capable by the capability file under FORGE_HOME/bin.
    let s = SelfDeploy::new();
    std::fs::write(
        s.bins.join("successor-capable"),
        format!("{}\n", std::process::id()),
    )
    .unwrap();
    let sha = s.commit("good");
    assert!(s.deploy_taken_over(&sha).status.success());
    assert_eq!(s.link("staged"), format!("releases/{sha}"));
    assert_eq!(worker_restarts(&s.calls()), 0, "{:?}", s.calls());

    // A capability file whose worker is gone is an older worker's home.
    let s = SelfDeploy::new();
    let mut dead = std::process::Command::new("true").spawn().unwrap();
    let pid = dead.id();
    dead.wait().unwrap();
    std::fs::write(s.bins.join("successor-capable"), format!("{pid}\n")).unwrap();
    let sha = s.commit("good");
    assert!(s.deploy(&sha).status.success());
    assert_eq!(worker_restarts(&s.calls()), 1, "{:?}", s.calls());
}

#[test]
fn a_successors_start_leaves_the_live_predecessors_proxy_dir_and_sweeps_a_dead_ones() {
    let e = Env::new();
    let root = e.home.join("bin");
    let run = e.home.join("run");
    std::fs::create_dir_all(&run).unwrap();
    for id in ["old", "new"] {
        let dir = root.join("releases").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let built = std::path::Path::new(env!("CARGO_BIN_EXE_forge"))
            .parent()
            .unwrap();
        for bin in ["forge", "forge-repomap"] {
            std::fs::copy(built.join(bin), dir.join(bin)).unwrap();
        }
    }
    std::os::unix::fs::symlink("releases/old", root.join("current")).unwrap();
    let systemctl_dir = e.home.join("fakebin");
    std::fs::create_dir_all(&systemctl_dir).unwrap();
    let systemctl = systemctl_dir.join("systemctl");
    std::fs::write(
        &systemctl,
        "#!/bin/bash\nif [ \"$2\" = is-active ]; then echo active; fi\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(
        &systemctl,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let path = format!(
        "{}:{}",
        systemctl_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let first = e.add(&["--retries", "0"]);
    let mut cmd = std::process::Command::new(root.join("releases/old/forge"));
    cmd.envs(
        e.cmd("slow-ok.sh")
            .get_envs()
            .filter_map(|(k, v)| Some((k, v?))),
    )
    .env("PATH", &path)
    .args(["work", "--poll", "1"]);
    let mut old = Worker::spawn(&mut cmd);
    let _reap = Reap(e.home.clone());
    assert!(
        wait_until(|| running_pid(&e, first).is_some(), Duration::from_secs(30)),
        "the old worker never claimed task {first}"
    );

    // A live worker's directory (a stand-in process, so the old worker's own
    // `mkdir` of its directory cannot collide with ours), and one left by a
    // worker that died.
    let mut alive = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    gone.wait().unwrap();
    let live = run.join(format!("egress-{}", alive.id()));
    let dead = run.join(format!("egress-{}", gone.id()));
    std::fs::create_dir(&live).unwrap();
    std::fs::create_dir(&dead).unwrap();

    std::os::unix::fs::symlink("releases/new", root.join("staged")).unwrap();
    let second = e.add(&["--retries", "0"]);
    assert!(
        wait_until(
            || running_pid(&e, second).is_some(),
            Duration::from_secs(30)
        ),
        "the successor never claimed task {second}"
    );
    assert!(
        live.is_dir(),
        "the successor removed the live predecessor's directory"
    );
    assert!(!dead.exists(), "the dead worker's directory was not swept");
    assert!(old.wait().success());
    alive.kill().unwrap();
    alive.wait().unwrap();
}

/// A fake `systemctl` with no `forge-portal` unit, whose other units
/// answer `is-active` with the word in `<home>/units/<unit>`, and which
/// records every call.
fn fake_systemctl_without_portal(e: &Env) -> String {
    let fakes = e.home.join("fakebin");
    std::fs::create_dir_all(&fakes).unwrap();
    std::fs::create_dir_all(e.home.join("units")).unwrap();
    let systemctl = fakes.join("systemctl");
    std::fs::write(
        &systemctl,
        format!(
            r#"#!/bin/bash
echo "systemctl $*" >> "{calls}"
unit="${{@: -1}}"
if [ "$2" = restart ] && [ "$unit" = forge-portal ]; then
  echo "Failed to restart forge-portal.service: Unit forge-portal.service not found." >&2
  exit 5
fi
if [ "$2" = is-active ]; then cat "{units}/$unit" 2>/dev/null || echo inactive; fi
exit 0
"#,
            calls = e.home.join("calls.log").display(),
            units = e.home.join("units").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &systemctl,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    format!(
        "{}:{}",
        fakes.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Declare the self deploy target with `args` (`--arg` values).
fn declare_self_target(e: &Env, args: &[&str]) {
    let repo = e.repo.to_str().unwrap();
    let o = e.forge(
        "ok.sh",
        &["project", "new", "forge", "--purpose", "p", "--repo", repo],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut cmd = vec![
        "project",
        "deploy",
        "add",
        "forge",
        "self",
        "--repo",
        repo,
        "--method",
        "deploy-self",
    ];
    for a in args {
        cmd.extend(["--arg", a]);
    }
    let o = e.forge("ok.sh", &cmd);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// Run a successor on release `new` (current is `old`) until it has taken
/// over; returns what it logged and the `current` it left.
fn takeover(e: &Env, path: &str) -> (String, String) {
    let root = e.home.join("bin");
    for id in ["old", "new"] {
        std::fs::create_dir_all(root.join("releases").join(id)).unwrap();
    }
    std::os::unix::fs::symlink("releases/old", root.join("current")).unwrap();
    let log = e.home.join("successor.log");
    let mut cmd = e.cmd("ok.sh");
    cmd.env("PATH", path)
        .env("FORGE_RELEASE", "new")
        .env("FORGE_SUCCESSOR_OF", "1")
        .env_remove("NOTIFY_SOCKET")
        .stderr(std::fs::File::create(&log).unwrap())
        .args(["work", "--poll", "1"]);
    let mut w = Worker::spawn(&mut cmd);
    let _reap = Reap(e.home.clone());
    assert!(
        wait_until(
            || root.join("successor-capable").exists(),
            Duration::from_secs(30)
        ),
        "the successor never finished taking over: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    w.signal(libc::SIGTERM);
    let _ = w.wait();
    let current = std::fs::read_link(root.join("current")).unwrap();
    (
        std::fs::read_to_string(&log).unwrap(),
        current.display().to_string(),
    )
}

#[test]
fn a_successor_restarts_the_units_the_self_target_declares_and_a_missing_one_is_a_note() {
    let e = Env::new();
    declare_self_target(&e, &["units=forge-web forge-portal", "tries=2"]);
    let path = fake_systemctl_without_portal(&e);
    std::fs::write(e.home.join("units/forge-web"), "active\n").unwrap();
    let (log, current) = takeover(&e, &path);
    assert!(log.contains("restarted forge-web"), "{log}");
    assert!(log.contains("forge-portal: unit not found"), "{log}");
    assert!(!log.contains("could not restart"), "{log}");
    assert_eq!(current, "releases/new", "{log}");
    let calls = std::fs::read_to_string(e.home.join("calls.log")).unwrap();
    assert!(
        calls.contains("systemctl --user restart --no-block forge-web"),
        "{calls}"
    );
    assert!(
        calls.contains("systemctl --user restart --no-block forge-portal"),
        "{calls}"
    );
}

#[test]
fn a_successor_restarts_only_forge_web_when_the_target_declares_no_units() {
    let e = Env::new();
    declare_self_target(&e, &[]);
    let path = fake_systemctl_without_portal(&e);
    std::fs::write(e.home.join("units/forge-web"), "active\n").unwrap();
    let (log, current) = takeover(&e, &path);
    assert!(log.contains("restarted forge-web"), "{log}");
    assert!(!log.contains("forge-portal"), "{log}");
    assert_eq!(current, "releases/new", "{log}");
    let calls = std::fs::read_to_string(e.home.join("calls.log")).unwrap();
    assert!(!calls.contains("forge-portal"), "{calls}");
}

#[test]
fn a_unit_that_does_not_come_back_active_flips_current_back() {
    let e = Env::new();
    declare_self_target(&e, &["units=forge-web forge-portal", "tries=2"]);
    let path = fake_systemctl_without_portal(&e);
    std::fs::write(e.home.join("units/forge-web"), "inactive\n").unwrap();
    let (log, current) = takeover(&e, &path);
    assert!(
        log.contains("forge-web: did not become active within 2 tries"),
        "{log}"
    );
    assert!(log.contains("forge-portal: unit not found"), "{log}");
    assert!(log.contains("putting current back to old"), "{log}");
    assert_eq!(current, "releases/old", "{log}");
    let calls = std::fs::read_to_string(e.home.join("calls.log")).unwrap();
    let web_restarts = calls
        .lines()
        .filter(|c| c.contains("restart") && c.ends_with("forge-web"))
        .count();
    assert_eq!(
        web_restarts, 2,
        "restarted again on the old release: {calls}"
    );
}

fn plugin_row(e: &Env) -> String {
    let out = e.forge("ok.sh", &["doctor"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.contains("plugins"))
        .unwrap_or_default()
        .to_string()
}

fn keeper_status(e: &Env) -> serde_json::Value {
    let o = e.forge("ok.sh", &["plugin", "status", "keeper", "--json"]);
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn a_successor_that_dies_after_claiming_gives_the_plugins_back_to_the_worker_that_still_claims() {
    let e = Env::new();
    let plugin = e.home.join("plugins/keeper");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.toml"),
        "name = \"keeper\"\nrun = [\"./run.sh\"]\ncapabilities = [\"events\"]\nrestart = \"always\"\n",
    )
    .unwrap();
    std::fs::write(plugin.join("run.sh"), "#!/bin/bash\nexec sleep 300\n").unwrap();
    std::fs::set_permissions(
        plugin.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    assert!(
        e.forge("ok.sh", &["plugin", "enable", "keeper"])
            .status
            .success()
    );

    // A successor that takes the capability file, as a claiming worker
    // does, and is gone four seconds later.
    let root = e.home.join("bin");
    let bin = root.join("releases/new/forge");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(
        &bin,
        "#!/bin/sh\necho $$ > \"$FORGE_HOME/bin/successor-capable\"\nexec sleep 4\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let first = e.add(&["--retries", "0"]);
    let mut old = Worker::spawn(
        e.cmd("slow-ok.sh")
            .env("FORGE_RELEASE", "old")
            .args(["work", "--poll", "1"]),
    );
    let _reap = Reap(e.home.clone());
    let running = |e: &Env| keeper_status(e)["state"] == "running";
    assert!(
        wait_until(
            || running_pid(&e, first).is_some() && running(&e),
            Duration::from_secs(30)
        ),
        "the worker never claimed task {first} with the plugin up"
    );
    let before = keeper_status(&e)["pid"].as_i64().unwrap();

    std::os::unix::fs::symlink("releases/new", root.join("staged")).unwrap();
    let handed = |e: &Env| {
        let s = keeper_status(e);
        (s["state"] == "stopped")
            .then(|| s["last_exit"].as_str().unwrap_or_default().to_string())
            .filter(|t| t.contains("handoff"))
    };
    assert!(
        wait_until(|| handed(&e).is_some(), Duration::from_secs(30)),
        "the plugin was never handed to the successor that claimed"
    );
    let successor = std::fs::read_to_string(root.join("successor-capable"))
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(
        handed(&e).unwrap(),
        format!("stopped by worker for handoff to pid {successor}")
    );
    assert!(
        !plugin_row(&e).starts_with("FAIL"),
        "a plugin handed to a live successor is not unattended: {}",
        plugin_row(&e)
    );

    // The successor exits; the worker that still claims takes them back.
    assert!(
        wait_until(
            || running(&e) && keeper_status(&e)["pid"].as_i64() != Some(before),
            Duration::from_secs(30)
        ),
        "the plugin was not restarted after the successor died: {}",
        keeper_status(&e)
    );
    assert_eq!(running_pid(&e, first), Some(i64::from(old.id())));
    assert!(
        plugin_row(&e).starts_with("OK"),
        "status row: {}",
        plugin_row(&e)
    );
    old.stop();
}

#[test]
fn a_successor_that_claims_and_dies_is_not_started_again_by_a_restarted_worker() {
    let e = Env::new();
    let root = e.home.join("bin");
    let starts = e.home.join("starts.log");
    let bin = root.join("releases/new/forge");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho start >> \"{}\"\necho $$ > \"$FORGE_HOME/bin/successor-capable\"\nsleep 1\nexit 1\n",
            starts.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("releases/new", root.join("staged")).unwrap();

    // The worker hands over and exits 0; the successor dies a second later.
    let restart = |cmd: &mut std::process::Command| {
        cmd.env("FORGE_RELEASE", "old")
            .env("NOTIFY_SOCKET", e.home.join("missing.sock"))
            .env_remove("FORGE_SUCCESSOR_OF")
            .args(["work", "--poll", "1"]);
    };
    let mut cmd = e.cmd("ok.sh");
    restart(&mut cmd);
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::thread::sleep(Duration::from_millis(1500));
    // Then two `Restart=on-failure` restarts on `current`: they run until told to stop.
    for _ in 0..2 {
        let _ = std::fs::remove_file(root.join("successor-capable"));
        let mut cmd = e.cmd("ok.sh");
        restart(&mut cmd);
        let mut w = Worker::spawn(&mut cmd);
        std::thread::sleep(Duration::from_secs(3));
        w.signal(libc::SIGTERM);
        assert!(w.wait().success());
    }
    let started = std::fs::read_to_string(&starts).unwrap().lines().count();
    assert_eq!(started, 1, "the failed release was started again");
    let failed = std::fs::read_to_string(root.join("staged-failed")).unwrap();
    assert_eq!(failed.split_whitespace().next(), Some("new"), "{failed}");
    assert!(
        std::fs::symlink_metadata(root.join("staged")).is_err(),
        "staged was not retired"
    );
}

#[test]
fn a_successor_that_dies_after_a_restarted_worker_joined_is_not_started_again() {
    let e = Env::new();
    let root = e.home.join("bin");
    let starts = e.home.join("starts.log");
    let bin = root.join("releases/new/forge");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho start >> \"{}\"\necho $$ > \"$FORGE_HOME/bin/successor-capable\"\nsleep 3\nexit 1\n",
            starts.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("releases/new", root.join("staged")).unwrap();

    let restart = |cmd: &mut std::process::Command| {
        cmd.env("FORGE_RELEASE", "old")
            .env("NOTIFY_SOCKET", e.home.join("missing.sock"))
            .env_remove("FORGE_SUCCESSOR_OF")
            .args(["work", "--poll", "1"]);
    };
    let mut cmd = e.cmd("ok.sh");
    restart(&mut cmd);
    // Output goes to a file: the successor inherits it, and a pipe would
    // hold `output()` until the successor died.
    let log = std::fs::File::create(e.home.join("worker1.log")).unwrap();
    let status = cmd
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "{}",
        std::fs::read_to_string(e.home.join("worker1.log")).unwrap_or_default()
    );
    // No pause: the restarted workers join while the successor is still
    // alive, and it dies a moment later.
    for _ in 0..1 {
        let _ = std::fs::remove_file(root.join("successor-capable"));
        let mut cmd = e.cmd("ok.sh");
        restart(&mut cmd);
        let mut w = Worker::spawn(&mut cmd);
        std::thread::sleep(Duration::from_secs(6));
        w.signal(libc::SIGTERM);
        assert!(w.wait().success());
    }
    let started = std::fs::read_to_string(&starts).unwrap().lines().count();
    assert_eq!(started, 1, "the failed release was started again");
    let failed = std::fs::read_to_string(root.join("staged-failed")).unwrap();
    assert_eq!(failed.split_whitespace().next(), Some("new"), "{failed}");
    assert!(
        std::fs::symlink_metadata(root.join("staged")).is_err(),
        "staged was not retired"
    );
}
