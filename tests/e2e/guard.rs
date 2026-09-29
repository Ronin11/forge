//! The landing guard (deploy/pre-receive.guard, `crate::guard`): a
//! pre-receive hook that rejects a hand push to the base branch (and its
//! deletion) unless it carries the integrator's own push option, and a
//! loud emergency override for the rare case that must bypass it.

use crate::support::*;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

const HOOK: &str = include_str!("../../deploy/pre-receive.guard");

/// The hook written by hand into `e.origin`, pointed at `e.home` and
/// `e.repo`, with push options advertised: what `forge project guard`
/// itself would set up, done without going through the CLI so the plain
/// git-level behavior can be tested on its own.
fn install_by_hand(e: &Env) {
    let hook = e.origin.join("hooks/pre-receive");
    std::fs::write(&hook, HOOK).unwrap();
    let mut perm = std::fs::metadata(&hook).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&hook, perm).unwrap();
    git(
        &e.origin,
        &["config", "receive.advertisePushOptions", "true"],
    );
    git(
        &e.origin,
        &["config", "forge.home", e.home.to_str().unwrap()],
    );
    git(
        &e.origin,
        &["config", "forge.repo", e.repo.to_str().unwrap()],
    );
    git(&e.origin, &["config", "forge.base-branch", "main"]);
}

/// `$FORGE_HOME/bin/current/forge`, so the hook's own callback
/// (`forge guard record-override`) can find the binary this test built,
/// the way a real release layout would.
fn link_forge_binary(e: &Env) {
    let dir = e.home.join("bin/current");
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("forge");
    let _ = std::fs::remove_file(&dest);
    symlink(env!("CARGO_BIN_EXE_forge"), &dest).unwrap();
}

fn push(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("push")
        .args(args)
        .output()
        .expect("git push")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn origin_sha(e: &Env, branch: &str) -> String {
    Command::new("git")
        .args([
            "--git-dir",
            e.origin.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
        ])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn hand_branch(repo: &Path, name: &str, files: &[(&str, &str)]) -> String {
    git(repo, &["checkout", "-q", "-b", name, "main"]);
    for (path, text) in files {
        std::fs::write(repo.join(path), text).unwrap();
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", &format!("{name} by hand")]);
    let sha = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["checkout", "-q", "main"]);
    sha
}

#[test]
fn a_plain_push_to_the_base_is_rejected_with_the_adopt_message() {
    let e = Env::new();
    install_by_hand(&e);
    let o = push(&e.repo, &["origin", "main"]);
    assert!(!o.status.success());
    assert!(origin_sha(&e, "main").is_empty(), "nothing landed");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("master is landed by Forge"), "{err}");
    assert!(
        err.contains(&format!("forge adopt {} main", e.repo.to_str().unwrap())),
        "{err}"
    );
}

#[test]
fn a_push_to_another_branch_is_accepted() {
    let e = Env::new();
    install_by_hand(&e);
    git(&e.repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(e.repo.join("f.txt"), "x").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "feature work"]);
    let o = push(&e.repo, &["origin", "feature"]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(
        origin_sha(&e, "feature"),
        git(&e.repo, &["rev-parse", "HEAD"])
    );
}

#[test]
fn deleting_the_base_branch_is_rejected_too() {
    let e = Env::new();
    let sha = git(&e.repo, &["rev-parse", "HEAD"]);
    // Seed the base before installing the guard so there is something to delete.
    let seeded = push(&e.repo, &["origin", "main"]);
    assert!(seeded.status.success(), "{}", text(&seeded));
    install_by_hand(&e);
    assert_eq!(origin_sha(&e, "main"), sha);
    let o = push(&e.repo, &["origin", "--delete", "main"]);
    assert!(!o.status.success());
    assert_eq!(origin_sha(&e, "main"), sha, "the base still exists");
}

#[test]
fn forge_project_guard_installs_the_hook_and_the_integrators_landing_succeeds() {
    assert_guard_landing(false, None);
}

#[test]
fn a_relative_origin_is_guarded_and_reported_and_accepts_integrator_landings() {
    assert_guard_landing(true, None);
}

#[test]
fn a_relative_hooks_path_guard_is_reported_and_accepts_integrator_landings() {
    assert_guard_landing(false, Some(false));
}

#[test]
fn an_absolute_hooks_path_guard_is_reported_and_accepts_integrator_landings() {
    assert_guard_landing(false, Some(true));
}

fn assert_guard_landing(relative: bool, absolute_hooks: Option<bool>) {
    let e = Env::new();
    let hooks = if let Some(absolute) = absolute_hooks {
        let path = e.origin.join("custom hooks");
        std::fs::create_dir_all(&path).unwrap();
        let setting = if absolute {
            path.to_str().unwrap()
        } else {
            "custom hooks"
        };
        git(&e.origin, &["config", "core.hooksPath", setting]);
        // A stale default hook must not hide a missing effective hook.
        std::fs::write(e.origin.join("hooks/pre-receive"), HOOK).unwrap();
        path
    } else {
        e.origin.join("hooks")
    };
    if relative {
        git(&e.repo, &["remote", "set-url", "origin", "../origin.git"]);
    }
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "new",
            "guarded",
            "--purpose",
            "A repository the landing guard protects.",
            "--repo",
            e.repo.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", text(&o));

    let doc = text(&e.forge("ok.sh", &["doctor"]));
    assert!(doc.contains("no landing guard: guarded"), "{doc}");

    let o = e.forge("ok.sh", &["project", "guard", "guarded"]);
    assert!(o.status.success(), "{}", text(&o));
    let doc = text(&e.forge("ok.sh", &["doctor"]));
    assert!(!doc.contains("no landing guard: guarded"), "{doc}");
    let hook = hooks.join("pre-receive");
    assert_eq!(std::fs::read_to_string(&hook).unwrap(), HOOK);
    let mode = std::fs::metadata(&hook).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the hook is executable");
    assert_eq!(
        git(&e.origin, &["config", "receive.advertisePushOptions"]),
        "true"
    );
    assert_eq!(
        git(&e.origin, &["config", "forge.repo"]),
        e.repo.to_str().unwrap()
    );

    // A plain hand push is still rejected once the guard is live: a new
    // commit, so the push actually asks to move the ref rather than
    // finding it already up to date.
    std::fs::write(e.repo.join("bypass.txt"), "x").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "bypass attempt"]);
    let bypass = push(&e.repo, &["origin", "main"]);
    assert!(!bypass.status.success());
    git(&e.repo, &["reset", "-q", "--hard", "HEAD~1"]);

    // The integrator's own landing push carries the token and succeeds.
    let sha = hand_branch(&e.repo, "jetpack", &[("answer.txt", "42\n")]);
    git(&e.repo, &["push", "-q", "origin", "jetpack"]);
    let o = e.forge(
        "neverrun.sh",
        &["adopt", e.repo.to_str().unwrap(), "jetpack"],
    );
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(origin_sha(&e, "main"), sha);
}

#[test]
fn an_override_push_is_accepted_and_recorded_and_doctor_reports_it() {
    assert_override_recorded("on-call fix, ticket 123");
}

#[test]
fn an_override_reason_with_leading_dashes_is_recorded_verbatim() {
    assert_override_recorded("--force needed for incident 123");
}

fn assert_override_recorded(expected_reason: &str) {
    let e = Env::new();
    // `forge doctor` first, only so FORGE_HOME (the store, the config)
    // exists before the hook tries to write into it.
    let _ = e.forge("ok.sh", &["doctor", "--json"]);
    install_by_hand(&e);
    link_forge_binary(&e);

    let sha = git(&e.repo, &["rev-parse", "HEAD"]);
    let option = format!("forge-override={expected_reason}");
    let o = push(&e.repo, &["origin", "main", "-o", &option]);
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(origin_sha(&e, "main"), sha);

    let (repo, pusher, reason): (String, String, String) = e
        .db()
        .query_row(
            "SELECT repo, answered_by, answer FROM decisions WHERE kind='forge-override'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(repo, e.repo.to_str().unwrap());
    assert_eq!(reason, expected_reason);
    let whoami = String::from_utf8_lossy(&Command::new("id").arg("-un").output().unwrap().stdout)
        .trim()
        .to_string();
    assert_eq!(pusher, whoami);

    let doc = text(&e.forge("ok.sh", &["doctor"]));
    assert!(doc.contains("guard_overrides"), "{doc}");
    assert!(doc.contains(expected_reason), "{doc}");
}

#[test]
fn an_override_is_rejected_when_recording_the_decision_fails() {
    let e = Env::new();
    let _ = e.forge("ok.sh", &["doctor", "--json"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
    let original = origin_sha(&e, "main");
    install_by_hand(&e);
    link_forge_binary(&e);
    e.db()
        .execute_batch(
            "CREATE TRIGGER reject_override BEFORE INSERT ON decisions
             WHEN NEW.kind = 'forge-override'
             BEGIN SELECT RAISE(FAIL, 'override recording unavailable'); END;",
        )
        .unwrap();
    std::fs::write(e.repo.join("incident.txt"), "fix").unwrap();
    git(&e.repo, &["add", "incident.txt"]);
    git(&e.repo, &["commit", "-qm", "incident fix"]);

    for refspec in ["main", ":main"] {
        let o = push(
            &e.repo,
            &["origin", refspec, "-o", "forge-override=incident"],
        );
        assert!(!o.status.success(), "{}", text(&o));
        assert!(text(&o).contains("could not record the emergency override"));
        assert_eq!(origin_sha(&e, "main"), original);
    }
    let count: i64 = e
        .db()
        .query_row(
            "SELECT count(*) FROM decisions WHERE kind='forge-override'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn guard_protects_all_separate_push_urls_and_doctor_checks_each() {
    assert_push_destinations_guarded(false);
}

#[test]
fn guard_protects_a_push_instead_of_destination() {
    assert_push_destinations_guarded(true);
}

fn assert_push_destinations_guarded(rewrite: bool) {
    let e = Env::new();
    let first = e.repo.parent().unwrap().join("push-one.git");
    let second = e.repo.parent().unwrap().join("push-two.git");
    for bare in [&first, &second] {
        git(
            e.repo.parent().unwrap(),
            &["init", "--bare", bare.to_str().unwrap()],
        );
    }
    if rewrite {
        git(
            &e.repo,
            &[
                "config",
                &format!("url.{}.pushInsteadOf", first.display()),
                e.origin.to_str().unwrap(),
            ],
        );
    } else {
        for url in ["../push-one.git", "../push-two.git"] {
            git(
                &e.repo,
                &["remote", "set-url", "--add", "--push", "origin", url],
            );
        }
    }
    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "new",
            "guarded",
            "--purpose",
            "Protect push destinations.",
            "--repo",
            e.repo.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    // An already protected fetch destination must not conceal missing push hooks.
    install_by_hand(&e);
    let doc = text(&e.forge("ok.sh", &["doctor"]));
    assert!(doc.contains("no landing guard: guarded"), "{doc}");
    let o = e.forge("ok.sh", &["project", "guard", "guarded"]);
    assert!(o.status.success(), "{}", text(&o));
    let doc = text(&e.forge("ok.sh", &["doctor"]));
    assert!(!doc.contains("no landing guard: guarded"), "{doc}");
    let destinations = if rewrite {
        vec![&first]
    } else {
        vec![&first, &second]
    };
    let o = push(&e.repo, &["origin", "main"]);
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("forge adopt"), "{}", text(&o));
    for bare in &destinations {
        assert_eq!(
            std::fs::read_to_string(bare.join("hooks/pre-receive")).unwrap(),
            HOOK
        );
        assert!(git(bare, &["for-each-ref", "refs/heads/main"]).is_empty());
    }
    let o = push(&e.repo, &["origin", "main:refs/heads/feature"]);
    assert!(o.status.success(), "{}", text(&o));
    let token = std::fs::read_to_string(e.home.join("forge-integrator.token")).unwrap();
    let o = push(
        &e.repo,
        &[
            "-o",
            &format!("forge-integrator={}", token.trim()),
            "origin",
            "main",
        ],
    );
    assert!(o.status.success(), "{}", text(&o));
    for bare in &destinations {
        assert_eq!(
            git(bare, &["rev-parse", "main"]),
            git(&e.repo, &["rev-parse", "main"])
        );
    }
    // Doctor must inspect the last push URL too, not only the first one.
    std::fs::remove_file(destinations.last().unwrap().join("hooks/pre-receive")).unwrap();
    let doc = text(&e.forge("ok.sh", &["doctor"]));
    assert!(doc.contains("no landing guard: guarded"), "{doc}");
}
