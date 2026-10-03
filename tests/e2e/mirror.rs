//! The bare origin's post-update mirror hook (deploy/post-update.mirror,
//! REVIEW-4 section 3.3): `forge init --mirror <remote>` installs it into
//! a registered repository's bare origin, a push is accepted at once when
//! the mirror is unreachable or hangs, and a reachable mirror receives
//! `main` and `v*` tags and nothing else.

use crate::support::*;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const HOOK: &str = include_str!("../../deploy/post-update.mirror");

/// `git push` from `repo`, timed: the hook must never hold it open.
fn timed_push(repo: &Path, args: &[&str]) -> (std::process::Output, Duration) {
    let t0 = Instant::now();
    let o = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("push")
        .args(args)
        .output()
        .expect("git push");
    (o, t0.elapsed())
}

/// The hook written by hand into `bare`, pointed at `mirror`.
fn hook_by_hand(bare: &Path, mirror: &str) {
    let hook = bare.join("hooks/post-update");
    std::fs::write(&hook, HOOK).unwrap();
    let mut perm = std::fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&hook, perm).unwrap();
    git(bare, &["config", "forge.mirror", mirror]);
}

/// `forge init --mirror <mirror>` with no systemd session reachable.
fn init_mirror(e: &Env, mirror: &str) -> std::process::Output {
    let o = e
        .cmd("ok.sh")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .args(["init", "--mirror", mirror])
        .output()
        .expect("forge init");
    eprintln!(
        "--- forge init --mirror {mirror} ---\n{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

#[test]
fn a_bare_origin_with_the_hook_accepts_a_push_when_the_mirror_is_unreachable() {
    let e = Env::new();
    // Nothing listens on the discard port: the mirror's push fails.
    hook_by_hand(&e.origin, "http://127.0.0.1:9/unreachable.git");
    let (o, _) = timed_push(&e.repo, &["origin", "main"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        git(&e.origin, &["rev-parse", "main"]),
        git(&e.repo, &["rev-parse", "HEAD"])
    );
    let log = e.origin.join("mirror.log");
    assert!(
        wait_until(
            || std::fs::read_to_string(&log).is_ok_and(|l| l.contains("--- exit")),
            Duration::from_secs(30)
        ),
        "the mirror's failure is logged in the bare repository"
    );
}

#[test]
fn a_mirror_that_never_answers_does_not_hold_the_push_open() {
    let e = Env::new();
    // Accepts connections and never replies: a hung GitHub.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    hook_by_hand(&e.origin, &format!("http://127.0.0.1:{port}/hung.git"));
    let (o, took) = timed_push(&e.repo, &["origin", "main"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(took < Duration::from_secs(20), "push took {took:?}");
    drop(listener);
}

#[test]
fn forge_init_mirror_installs_the_hook_and_mirrors_main_and_v_tags_only() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "new",
            "mirrored",
            "--purpose",
            "A repository whose origin mirrors to another.",
            "--repo",
            e.repo.to_str().unwrap(),
        ],
    );
    assert!(o.status.success());
    let mirror = e.origin.with_file_name("mirror.git");
    git(
        e.origin.parent().unwrap(),
        &["init", "-q", "--bare", mirror.to_str().unwrap()],
    );

    let o = init_mirror(&e, mirror.to_str().unwrap());
    assert!(o.status.success());
    let hook = e.origin.join("hooks/post-update");
    assert_eq!(std::fs::read_to_string(&hook).unwrap(), HOOK);
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&hook).unwrap().permissions());
    assert_eq!(mode & 0o111, 0o111, "the hook is executable");
    assert_eq!(
        git(&e.origin, &["config", "forge.mirror"]),
        mirror.to_str().unwrap()
    );
    let again = init_mirror(&e, mirror.to_str().unwrap());
    let out = String::from_utf8_lossy(&again.stdout);
    assert!(out.contains("already initialized"), "{out}");

    git(&e.repo, &["tag", "v1.0.0"]);
    git(&e.repo, &["tag", "scratch"]);
    git(&e.repo, &["branch", "side"]);
    // `forge init` also guards the base branch (see `guard`): a hand push
    // of `main` needs the integrator's own token, which `forge init`
    // provisioned at `FORGE_HOME/forge-integrator.token` alongside the hook.
    let token = std::fs::read_to_string(e.home.join("forge-integrator.token")).unwrap();
    let (o, _) = timed_push(
        &e.repo,
        &[
            "origin",
            "main",
            "side",
            "v1.0.0",
            "scratch",
            "-o",
            &format!("forge-integrator={}", token.trim()),
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let head = git(&e.repo, &["rev-parse", "HEAD"]);
    assert!(
        wait_until(
            || {
                let refs = git(&mirror, &["for-each-ref", "--format=%(refname)"]);
                refs.contains("refs/heads/main") && refs.contains("refs/tags/v1.0.0")
            },
            Duration::from_secs(30)
        ),
        "main and v1.0.0 reach the mirror"
    );
    assert_eq!(git(&mirror, &["rev-parse", "main"]), head);
    let refs = git(&mirror, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        !refs.contains("side") && !refs.contains("scratch"),
        "{refs}"
    );
}

#[test]
fn forge_init_mirror_with_no_bare_origin_registered_fails() {
    let e = Env::new();
    let o = init_mirror(&e, "https://github.com/example/x.git");
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("no registered repository"));
}
