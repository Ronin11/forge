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
        "#!/bin/bash\necho \"systemctl $*\" >> \"$SUCC_CALLS_LOG\"\n",
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
        calls.contains("systemctl --user restart --no-block forge-web forge-portal"),
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
    let o = s.deploy(&sha);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(s.link("staged"), format!("releases/{sha}"));
    // The successor flips current; the deploy leaves it alone.
    assert_eq!(s.link("current"), "releases/old");
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
    assert!(s.deploy(&sha).status.success());
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
    let tmp = e.home.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
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
    std::fs::write(&systemctl, "#!/bin/bash\nexit 0\n").unwrap();
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
    .env("TMPDIR", &tmp)
    .args(["work", "--poll", "1"]);
    let mut old = Worker::spawn(&mut cmd);
    let _reap = Reap(e.home.clone());
    assert!(
        wait_until(|| running_pid(&e, first).is_some(), Duration::from_secs(30)),
        "the old worker never claimed task {first}"
    );
    let old_pid = running_pid(&e, first).unwrap();

    // The live predecessor's directory, and one left by a worker that died.
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    gone.wait().unwrap();
    let live = tmp.join(format!("forge-egress-{old_pid}"));
    let dead = tmp.join(format!("forge-egress-{}", gone.id()));
    // The old worker makes its own once it starts the task: either may win.
    std::fs::create_dir_all(&live).unwrap();
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
}
