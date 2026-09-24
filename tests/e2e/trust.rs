//! Trust by source enforcement at enqueue (docs/GTM.md item 1): a task
//! filed at a trust level is judged against that level's own
//! `[trust.<level>]` policy, checked in `queue::apply_trust_policy`.

use crate::support::Env;

#[test]
fn a_public_task_with_workflow_direct_is_refused() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--trust",
            "public",
            "--workflow",
            "direct",
        ],
    );
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("public"), "{stderr}");
    assert!(stderr.contains("direct"), "{stderr}");
    assert!(stderr.contains("reviewed"), "{stderr}");
}

#[test]
fn a_public_task_with_workflow_reviewed_is_filed_at_the_capped_budget() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--trust",
            "public",
            "--workflow",
            "reviewed",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();
    let budget: f64 = e
        .db()
        .query_row("SELECT budget_usd FROM tasks WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    // The default public policy's own budget_usd (src/config.rs's
    // DEFAULT_HOME_CONFIG template, [trust.public]).
    assert_eq!(budget, 1.0);
}

#[test]
fn a_public_task_naming_a_requested_budget_over_the_cap_is_filed_at_the_cap() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--trust",
            "public",
            "--workflow",
            "reviewed",
            "--budget",
            "50",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id: i64 = String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap();
    let budget: f64 = e
        .db()
        .query_row("SELECT budget_usd FROM tasks WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(budget, 1.0);
}

#[test]
fn a_public_task_with_allow_protected_is_refused() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--trust",
            "public",
            "--workflow",
            "reviewed",
            "--allow-protected",
        ],
    );
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("public"), "{stderr}");
    assert!(stderr.contains("allow-protected"), "{stderr}");
}

#[test]
fn an_operator_task_is_unrestricted_by_default() {
    let e = Env::new();
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--workflow",
            "direct",
            "--allow-protected",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn a_sixth_public_task_in_a_day_is_refused_by_per_day() {
    let e = Env::new();
    for _ in 0..5 {
        let o = e.forge(
            "ok.sh",
            &[
                "add",
                e.repo.to_str().unwrap(),
                "write 42 to answer.txt",
                "--trust",
                "public",
                "--workflow",
                "reviewed",
                "--no-land",
            ],
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let o = e.forge(
        "ok.sh",
        &[
            "add",
            e.repo.to_str().unwrap(),
            "write 42 to answer.txt",
            "--trust",
            "public",
            "--workflow",
            "reviewed",
            "--no-land",
        ],
    );
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("public"), "{stderr}");
    assert!(stderr.contains("5"), "{stderr}");
}

#[test]
fn a_public_task_ends_unverified_with_its_branch_pushed_and_forge_land_lands_it() {
    let e = Env::new();
    let mut c = e.cmd("ok.sh");
    for (role, fake) in [("REVIEW", "reviewer-ok.sh"), ("ASSESS", "assessor.sh")] {
        c.env(
            format!("FORGE_CLAUDE_BIN_{role}"),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fakes")
                .join(fake),
        );
    }
    let o = c
        .args([
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--trust",
            "public",
            "--workflow",
            "reviewed",
            "--retries",
            "0",
        ])
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "unverified", "{reason}");
    assert!(reason.contains("public"), "{reason}");
    assert!(pushed);
    assert!(e.origin_branches().contains("forge/1-write-42"));
    assert!(
        !e.origin_branches().contains("main"),
        "the base is untouched"
    );

    let o = c_land(&e);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let (state, reason, _) = e.task(1);
    assert_eq!(state, "unverified", "{reason}");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert_eq!(
        crate::support::origin_file(&e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
}

fn c_land(e: &Env) -> std::process::Output {
    let mut c = e.cmd("ok.sh");
    c.env(
        "FORGE_CLAUDE_BIN_ASSESS",
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fakes")
            .join("assessor.sh"),
    );
    c.args(["land", "1"]).output().unwrap()
}
