//! The agent login belongs to the kernel (src/login.rs): a refresh made in a
//! sandbox is written back over the host file, a near-expiry login is
//! refreshed on the host before a launch, and an empty one never seeds an
//! attempt.
use crate::support::*;
use std::path::{Path, PathBuf};
use std::process::Output;

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
    let config = config_dir(&e, &login("a0", "r0", 32503680000000));
    // The first attempt is seeded with r0, refreshes to r1 in its private
    // copy and answers wrong; the kernel writes r1 back over the host file.
    assert!(!run(&e, "login-refresh.sh", &config).status.success());
    assert_eq!(e.attempts(1)[0].1, "checks_failed");
    let host = std::fs::read_to_string(config.join(".credentials.json")).unwrap();
    assert!(
        host.contains("r1") && host.contains("32503680001000"),
        "the host seed carries the later stamp: {host}"
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

#[test]
fn an_empty_login_is_a_provider_refusal_not_an_attempt_and_doctor_fails_it() {
    let e = Env::new();
    let config = config_dir(&e, &login("", "", 0));
    let o = run(&e, "ok.sh", &config);
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
