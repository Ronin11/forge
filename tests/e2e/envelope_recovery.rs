use crate::support::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const WORK: &str = "echo 42 > answer.txt\ngit add answer.txt\ngit commit -qm answer";

fn run_variant(e: &Env, work: &str, extra: &[&str]) -> std::process::Output {
    let script = e.home.parent().unwrap().join("envelope-failure.sh");
    std::fs::write(
        &script,
        include_str!("../fakes/envelope-failure.sh").replace(WORK, work),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    e.cmd("envelope-failure.sh")
        .env("FORGE_CLAUDE_BIN", script)
        .args(["run", e.repo.to_str().unwrap(), "write 42"])
        .args(extra)
        .output()
        .unwrap()
}

fn assert_verified(e: &Env) {
    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @"), "{reason}");
    assert!(pushed);
    assert_eq!(
        origin_file(e, "main", "answer.txt").as_deref(),
        Some("42\n")
    );
    let a = e.attempts(1);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].1, "succeeded", "{}", a[0].2);
    for (level, name) in [
        ("L0", "result-structured"),
        ("L0", "clean-tree"),
        ("L1", "answer"),
        ("L2", "task-check-1"),
    ] {
        assert_eq!(check(&a[0].4, level, name), Some(true), "{}", a[0].4);
    }
    let raw: String = e
        .db()
        .query_row(
            "SELECT envelope_json FROM attempts WHERE task_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(envelope["summary"], "envelope missing; verified by checks");
    assert_eq!(envelope["changes"][0]["path"], "answer.txt");
    assert!(envelope["needs_input"].is_null());
}

#[test]
fn claude_commits_then_exhausts_envelope_retries_and_lands() {
    let e = Env::new();
    let o = e.forge(
        "envelope-failure.sh",
        &[
            "run",
            e.repo.to_str().unwrap(),
            "write 42",
            "--retries",
            "0",
            "--check",
            "test -f answer.txt",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_verified(&e);
    assert!(
        e.log_text(1, 1)
            .contains("structured_output_retry_exhausted")
    );
}

#[test]
fn envelope_failure_without_commits_names_the_structured_output_failure() {
    let e = Env::new();
    let o = run_variant(&e, "", &["--retries", "0"]);
    assert!(!o.status.success());
    let a = e.attempts(1);
    assert_eq!(a[0].1, "agent_failed");
    assert_eq!(
        a[0].2,
        "structured_output_retry_exhausted: envelope missing"
    );
    assert_eq!(check(&a[0].4, "L1", "answer"), None);
    assert!(origin_file(&e, "main", "answer.txt").is_none());
}

#[test]
fn envelope_recovery_keeps_l0_l1_and_l2_failures() {
    for (work, extra, level, name) in [
        (
            format!("{WORK}\necho dirty >> answer.txt"),
            vec![],
            "L0",
            "clean-tree",
        ),
        (WORK.replace("echo 42", "echo 41"), vec![], "L1", "answer"),
        (WORK.into(), vec!["--check", "false"], "L2", "task-check-1"),
    ] {
        let e = Env::new();
        let mut args = vec!["--retries", "0"];
        args.extend(extra);
        let o = run_variant(&e, &work, &args);
        assert!(!o.status.success(), "{level} {name}");
        let a = e.attempts(1);
        assert_eq!(a[0].1, "checks_failed", "{}", a[0].2);
        assert_eq!(check(&a[0].4, level, name), Some(false), "{}", a[0].4);
        assert!(origin_file(&e, "main", "answer.txt").is_none());
    }
}

#[test]
fn an_earlier_attempts_commits_do_not_recover_a_later_missing_envelope() {
    let e = Env::new();
    let work = format!(
        "if [ ! -f answer.txt ]; then\n{}\nfi",
        WORK.replace("echo 42", "echo 41")
    );
    let o = run_variant(&e, &work, &["--retries", "1"]);
    assert!(!o.status.success());
    let a = e.attempts(1);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].1, "checks_failed");
    assert_eq!(a[1].1, "agent_failed");
    assert_eq!(
        a[1].2,
        "structured_output_retry_exhausted: envelope missing"
    );
    assert_eq!(check(&a[1].4, "L1", "answer"), None);
}

#[test]
fn malformed_or_absent_envelopes_are_verified_even_without_a_result_frame() {
    for ending in [
        "echo '{\"type\":\"result\",\"is_error\":false,\"structured_output\":{\"bad\":true}}'\nexit 0",
        "echo '{not json'\nexit 0",
        "exit 1",
    ] {
        let e = Env::new();
        let o = run_variant(
            &e,
            &format!("{WORK}\n{ending}"),
            &["--retries", "0", "--check", "test -f answer.txt"],
        );
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_verified(&e);
    }
}

#[test]
fn codex_and_copilot_phase_two_envelope_failures_are_verified_and_land() {
    for (runner, bin, fake) in [
        ("codex-cli", "FORGE_CODEX_BIN", "codex-noenvelope.sh"),
        ("copilot-cli", "FORGE_COPILOT_BIN", "copilot-noenvelope.sh"),
    ] {
        let e = Env::new();
        std::fs::create_dir_all(&e.home).unwrap();
        std::fs::write(
            e.home.join("config.toml"),
            format!("[providers.fake]\nrunner = \"{runner}\"\n"),
        )
        .unwrap();
        let o = e
            .cmd("ok.sh")
            .env(
                bin,
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fakes")
                    .join(fake),
            )
            .args([
                "run",
                e.repo.to_str().unwrap(),
                "write 42",
                "--provider",
                "fake",
                "--retries",
                "0",
                "--check",
                "test -f answer.txt",
            ])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_verified(&e);
        assert!(e.log_text(1, 1).contains("forge_phase_two"));
    }
}
