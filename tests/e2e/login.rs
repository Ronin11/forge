//! The agent login belongs to the kernel (src/login.rs): a refresh made in a
//! sandbox is written back over the host file, a near-expiry login is
//! refreshed on the host before a launch, and an empty one never seeds an
//! attempt.
use crate::support::*;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn login(access: &str, refresh: &str, expires_at: i64) -> String {
    format!(
        r#"{{"claudeAiOauth":{{"accessToken":"{access}","refreshToken":"{refresh}","expiresAt":{expires_at}}}}}"#
    )
}

/// The operator's claude config directory, holding `creds`.
fn config_dir(e: &Env, creds: &str) -> PathBuf {
    let dir = e.home.join("operator-claude");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".credentials.json"), creds).unwrap();
    dir
}

fn forge(e: &Env, fake: &str, config: &Path, args: &[&str]) -> Output {
    let o = e
        .cmd(fake)
        .env("CLAUDE_CONFIG_DIR", config)
        .args(args)
        .output()
        .unwrap();
    eprintln!(
        "--- forge {} ---\n{}{}",
        args.join(" "),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

fn run(e: &Env, fake: &str, config: &Path) -> Output {
    let repo = e.repo.to_str().unwrap();
    let args = ["run", repo, "write 42 to answer.txt", "--no-land"];
    forge(e, fake, config, &[&args[..], &["--retries", "0"]].concat())
}

#[test]
fn a_token_a_sandbox_refreshed_is_written_back_and_seeds_the_next_attempt() {
    let e = Env::new();
    if e.sandbox_disabled() {
        eprintln!("FORGE_TEST_NO_SANDBOX=1: skipping, bwrap unavailable");
        return;
    }
    let seeded_at = now_ms() + 3 * 3600 * 1000;
    let config = config_dir(&e, &login("a0", "r0", seeded_at));
    // The first attempt is seeded with r0, refreshes to r1 in its private
    // copy and answers wrong; the kernel writes r1 back over the host file.
    assert!(!run(&e, "login-refresh.sh", &config).status.success());
    assert_eq!(e.attempts(1)[0].1, "checks_failed");
    let host = std::fs::read_to_string(config.join(".credentials.json")).unwrap();
    assert!(
        host.contains("r1") && !host.contains(&seeded_at.to_string()),
        "the host seed carries the later stamp: {host}"
    );
    let prev = std::fs::read_to_string(config.join(".credentials.json.forge-prev")).unwrap();
    assert_eq!(
        prev,
        login("a0", "r0", seeded_at),
        "the replaced login is kept"
    );
    assert!(
        std::fs::read_dir(&config)
            .unwrap()
            .flatten()
            .all(|f| !f.file_name().to_string_lossy().ends_with(".tmp")),
        "the atomic replace leaves no sibling behind"
    );
    // A second task's sandbox is seeded from the host file, so it sees r1.
    let o = run(&e, "login-refresh.sh", &config);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(2).0, "succeeded");
}

#[test]
fn a_pair_a_sandbox_forged_never_replaces_the_host_login() {
    let e = Env::new();
    if e.sandbox_disabled() {
        eprintln!("FORGE_TEST_NO_SANDBOX=1: skipping, bwrap unavailable");
        return;
    }
    let seeded = login("a0", "r0", now_ms() + 3 * 3600 * 1000);
    let config = config_dir(&e, &seeded);
    let o = run(&e, "login-forge.sh", &config);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(e.task(1).0, "succeeded");
    assert_eq!(
        std::fs::read_to_string(config.join(".credentials.json")).unwrap(),
        seeded,
        "the host login is as the operator left it"
    );
    assert!(
        !config.join(".credentials.json.forge-prev").exists(),
        "nothing was accepted, so nothing was backed up"
    );
    assert!(!config.join(".forge-writeback").exists());
}

#[test]
fn a_login_near_expiry_is_refreshed_on_the_host_before_the_attempt_is_seeded() {
    let e = Env::new();
    let soon = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64)
        + 10 * 60 * 1000;
    let config = config_dir(&e, &login("a0", "r0", soon));
    let o = run(&e, "login-probe.sh", &config);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        e.task(1).0,
        "succeeded",
        "the attempt saw the refreshed pair"
    );
    let probes = std::fs::read_to_string(config.join(".credentials.json.probes")).unwrap();
    assert_eq!(
        probes.lines().count(),
        1,
        "one probe, on the host: {probes}"
    );
}

/// Find `name` anywhere under `dir`, however deep.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

/// (docs/REVIEW-4.md #1.4) The probe used to run in the task's worktree,
/// with the repository's own `.claude/settings.json` (and any hook it
/// names) in force. A hook that writes a marker must never fire from it,
/// and its cwd must never be the worktree at all.
#[test]
fn the_login_refresh_probe_never_runs_in_the_tasks_worktree() {
    let e = Env::new();
    std::fs::create_dir_all(e.repo.join(".claude")).unwrap();
    std::fs::write(
        e.repo.join(".claude/settings.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo hooked >hook-marker.txt"}]}]}}"#,
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "a settings hook"]);
    let soon = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64)
        + 10 * 60 * 1000;
    let config = config_dir(&e, &login("a0", "r0", soon));
    let o = run(&e, "login-probe-cwd.sh", &config);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let cwd = std::fs::read_to_string(config.join(".credentials.json.probe-cwd")).unwrap();
    let cwd = cwd.trim();
    let worktree = e.home.join("worktrees");
    assert!(
        !Path::new(cwd).starts_with(&worktree) && cwd != e.repo.to_str().unwrap(),
        "the probe ran in the task's worktree: {cwd}"
    );
    assert_eq!(
        cwd,
        e.home.join("probe").to_str().unwrap(),
        "the probe runs in FORGE_HOME/probe"
    );
    assert!(
        find_file(&e.home, "hook-marker.txt").is_none(),
        "the settings hook fired from outside the worktree"
    );
}

#[test]
fn an_empty_login_is_a_provider_refusal_not_an_attempt_and_doctor_fails_it() {
    let e = Env::new();
    let config = config_dir(&e, &login("", "", 0));
    // An unusable login cannot recover on its own. Exercise the explicit
    // requeue policy so this test does not wait for operator intervention.
    let o = forge(
        &e,
        "ok.sh",
        &config,
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--no-land",
            "--no-wait",
            "--retries",
            "0",
        ],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("run `claude login`"), "{err}");
    let a = e.attempts(1);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].2, "rate limited by the provider");
    assert_eq!(e.task(1).0, "queued", "held, not failed");
    assert!(
        !e.repo.join("answer.txt").exists() && e.log_text(1, 1).contains("claude login"),
        "the agent never ran"
    );
    let o = forge(&e, "ok.sh", &config, &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.lines()
            .any(|l| l.contains("FAIL") && l.contains("anthropic") && l.contains("empty token")),
        "{out}"
    );
}

#[test]
fn doctor_shows_the_logins_expiry_and_a_recent_write_back() {
    let e = Env::new();
    let config = config_dir(&e, &login("a0", "r0", 32503680000000));
    let out = |e: &Env| {
        let o = forge(e, "ok.sh", &config, &["doctor"]);
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let before = out(&e);
    assert!(
        before.contains("OK   anthropic") && before.contains("no write-back in the last 8h"),
        "{before}"
    );
    assert!(before.contains("3000-01-01"), "{before}");
    std::fs::write(
        config.join(".forge-writeback"),
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 120)
            .to_string(),
    )
    .unwrap();
    assert!(out(&e).contains("write-back yes (2m ago)"));
}

/// 2026-09-26 23:48 to 02:24: an expired login answered every launch with
/// 'Failed to authenticate', and 40 attempts on 15 tasks were spent on it.
/// A refused login is a refusal: refunded, the provider held (announced
/// once), doctor red; the hold names no time and ends when a probe answers,
/// and the task then lands on the base it was queued with.
#[test]
fn an_expired_login_is_a_hold_and_a_doctor_fail_never_a_burned_attempt() {
    let e = Env::new();
    let id = e.add(&[]);
    let base = |e: &Env| -> (String, String) {
        e.db()
            .query_row(
                "SELECT base_sha, verify_base FROM tasks WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    let stderr_path = e.home.join("worker-stderr.log");
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();
    let mut worker = Worker::spawn(
        e.cmd("login-expired.sh")
            .args(["work", "--once"])
            .stderr(stderr_file),
    );
    let log = || std::fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        wait_until(
            || log().contains("login expired since") && log().contains("holding"),
            Duration::from_secs(30)
        ),
        "the worker never held the provider: {}",
        log()
    );
    worker.stop();
    let a = e.attempts(id);
    assert_eq!(a.len(), 1, "one launch, then the hold: {a:?}");
    assert_eq!(a[0].2, "the provider refused the agent login");
    let refunded: i64 = e
        .db()
        .query_row(
            "SELECT refunded FROM attempts WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(refunded, 1, "the refusal does not count as an attempt");
    let (state, reason, _) = e.task(id);
    assert_eq!(state, "queued", "{reason}");
    let queued_on = base(&e);
    let held: i64 = e
        .db()
        .query_row(
            "SELECT COUNT(*) FROM provider_holds WHERE provider='anthropic'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held, 1);
    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    assert_eq!(
        events.matches("\"type\":\"provider_held\"").count(),
        1,
        "the hold is announced once: {events}"
    );

    let o = e.forge("login-expired.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!o.status.success(), "{out}");
    assert!(
        out.lines().any(|l| l.contains("FAIL")
            && l.contains("anthropic")
            && l.contains("login expired since")
            && l.contains("run claude login as the operator, then forge doctor")),
        "{out}"
    );

    // The login answers again: the next worker's first probe releases the
    // hold, and the task runs on the base and suite it was queued with.
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(id);
    assert_eq!(state, "succeeded", "{reason}");
    assert_eq!(base(&e), queued_on);
    let a = e.attempts(id);
    assert_eq!((a.len(), a[1].1.as_str()), (2, "succeeded"), "{a:?}");
    let probes: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM provider_probes WHERE ok=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(probes, 1);
    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    assert!(
        events.contains("\"type\":\"provider_released\""),
        "{events}"
    );
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(!out.contains("login expired since"), "{out}");
}
