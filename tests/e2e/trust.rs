//! Trust by source enforcement at enqueue (docs/GTM.md item 1): a task
//! filed at a trust level is judged against that level's own
//! `[trust.<level>]` policy, checked in `queue::apply_trust_policy`.

use crate::support::{Env, git};

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
    // The default public policy's own per_task_usd (src/config.rs's
    // DEFAULT_HOME_CONFIG template, [trust.public]).
    assert_eq!(budget, 5.0);
}

fn add_public(e: &Env, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "add",
        e.repo.to_str().unwrap(),
        "write 42 to answer.txt",
        "--trust",
        "public",
        "--workflow",
        "reviewed",
    ];
    args.extend_from_slice(extra);
    e.forge("ok.sh", &args)
}

fn filed_id(o: &std::process::Output) -> i64 {
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn a_public_task_naming_a_budget_under_the_cap_keeps_it() {
    let e = Env::new();
    let o = add_public(&e, &["--budget", "2"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let budget: f64 = e
        .db()
        .query_row(
            "SELECT budget_usd FROM tasks WHERE id=?1",
            [filed_id(&o)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(budget, 2.0);
}

#[test]
fn a_public_task_naming_a_budget_over_the_cap_is_refused_without_the_flag() {
    let e = Env::new();
    let o = add_public(&e, &["--budget", "20"]);
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("public"), "{stderr}");
    assert!(stderr.contains("$5.00"), "{stderr}");
    assert!(stderr.contains("--allow-over-trust-cap"), "{stderr}");
    let n: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn allow_over_trust_cap_files_the_budget_and_records_a_decision() {
    let e = Env::new();
    let o = add_public(&e, &["--budget", "20", "--allow-over-trust-cap"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = filed_id(&o);
    let db = e.db();
    let budget: f64 = db
        .query_row("SELECT budget_usd FROM tasks WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(budget, 20.0);
    let (by, answer): (String, String) = db
        .query_row(
            "SELECT answered_by, answer FROM decisions WHERE task_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(by, "operator");
    assert!(
        answer.contains("$20.00") && answer.contains("$5.00"),
        "{answer}"
    );
}

#[test]
fn retry_holds_an_explicit_budget_to_the_trust_cap() {
    let e = Env::new();
    let o = add_public(&e, &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let id = filed_id(&o).to_string();
    let w = e.forge("ok.sh", &["withdraw", &id, "--reason", "x"]);
    assert!(w.status.success(), "{}", String::from_utf8_lossy(&w.stderr));

    let r = e.forge("ok.sh", &["retry", &id, "--budget", "50"]);
    assert!(!r.status.success());
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(stderr.contains("--allow-over-trust-cap"), "{stderr}");
    let n: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);

    let r = e.forge(
        "ok.sh",
        &["retry", &id, "--budget", "50", "--allow-over-trust-cap"],
    );
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let n: i64 = e
        .db()
        .query_row(
            "SELECT COUNT(*) FROM decisions WHERE answer LIKE '%--allow-over-trust-cap%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn task_set_holds_a_budget_edit_to_the_trust_cap() {
    let e = Env::new();
    let id = filed_id(&add_public(&e, &[]));
    let id = id.to_string();
    let o = e.forge("ok.sh", &["task", "set", &id, "--budget", "20"]);
    assert!(!o.status.success());
    let o = e.forge(
        "ok.sh",
        &[
            "task",
            "set",
            &id,
            "--budget",
            "20",
            "--allow-over-trust-cap",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn doctor_prints_the_three_levels_caps() {
    let e = Env::new();
    let o = e.forge("ok.sh", &["doctor"]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("public $5.00 a task / $25.00 an initiative"),
        "{out}"
    );
    assert!(
        out.contains("contact $10.00 a task / $50.00 an initiative"),
        "{out}"
    );
    assert!(
        out.contains("operator $2.00 a task / none an initiative"),
        "{out}"
    );
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
    assert_eq!(state, "succeeded", "{reason}");
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

fn declare_host_backend(e: &Env) {
    let path = e.repo.join("forge.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{config}[execution]\nbackend = \"host\"\n")).unwrap();
    git(&e.repo, &["commit", "-qam", "select host executor"]);
}

#[test]
fn a_public_task_is_refused_at_enqueue_on_a_repository_that_runs_on_the_host() {
    let e = Env::new();
    declare_host_backend(&e);
    let o = add_public(&e, &[]);
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("host"), "{stderr}");
    assert!(stderr.contains("public"), "{stderr}");
    assert!(stderr.contains("egress_bounded"), "{stderr}");
    let n: i64 = e
        .db()
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    // The operator's own task is not restricted.
    assert!(
        e.forge(
            "ok.sh",
            &["add", e.repo.to_str().unwrap(), "write 42 to answer.txt"]
        )
        .status
        .success()
    );
}

#[test]
fn a_queued_public_task_is_blocked_not_failed_when_its_repository_moves_to_the_host() {
    let e = Env::new();
    if e.sandbox_disabled() {
        return;
    }
    let public = filed_id(&add_public(&e, &["--no-land"]));
    declare_host_backend(&e);
    let operator = e.add(&["--no-land"]);
    let o = e.forge("ok.sh", &["work", "--once"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let (state, reason, _) = e.task(public);
    assert_eq!(state, "blocked", "{reason}");
    assert!(
        reason.contains("host") && reason.contains("public"),
        "{reason}"
    );
    assert!(reason.contains("allow_unsandboxed"), "{reason}");
    assert!(e.attempts(public).is_empty(), "no agent was launched");

    let (state, reason, _) = e.task(operator);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(!e.attempts(operator).is_empty());
}

#[test]
fn allow_unsandboxed_lets_a_public_task_start_on_the_host_and_doctor_flags_it() {
    let e = Env::new();
    declare_host_backend(&e);
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[trust.public]\nallow_unsandboxed = true\n",
    )
    .unwrap();
    let o = add_public(&e, &["--no-land"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.forge("ok.sh", &["doctor", "--json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&o.stdout).unwrap();
    let row = rows
        .iter()
        .find(|r| r["name"] == "trust.allow_unsandboxed")
        .expect("doctor flags the opt-out");
    assert_eq!(row["status"], "warn");
    assert!(row["detail"].as_str().unwrap().contains("public"));
}
