use crate::support::*;

/// `landing::deploy_on_landing` must not discard `deploy::run`'s error: an
/// on-landing target whose method is unknown errors in
/// `operation::resolve_deploy_method`, before `deploy::run` ever calls
/// `start_deploy`. The landing itself still succeeds (a deploy target's
/// own failure never changes the task's landed state), the error is named
/// on the task instead of dropped, and no deploy row is left unfinished
/// (there is none to leave open: the method never ran).
#[test]
fn an_on_landing_targets_method_that_errors_before_running_names_the_error_and_leaves_no_row_open()
{
    let e = Env::new();
    let repo_s = e.repo.to_str().unwrap();

    assert!(
        e.forge(
            "ok.sh",
            &["project", "new", "demo", "--purpose", "p", "--repo", repo_s],
        )
        .status
        .success()
    );

    let o = e.forge(
        "ok.sh",
        &[
            "project",
            "deploy",
            "add",
            "demo",
            "prod",
            "--repo",
            repo_s,
            "--method",
            "no-such-method",
            "--check",
            "true",
            "--on-landing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.forge("ok.sh", &["run", repo_s, "write 42", "--retries", "0"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let (state, reason, _) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @ "), "{reason}");

    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    let parsed: Vec<serde_json::Value> = events
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let note = parsed
        .iter()
        .find(|v| v["type"] == "note" && v["text"].as_str().is_some_and(|t| t.contains("prod")))
        .unwrap_or_else(|| panic!("no deploy-failure note in:\n{events}"));
    assert_eq!(note["task"], 1, "{note}");
    let text = note["text"].as_str().unwrap();
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("unknown deploy method"), "{text}");

    let rows: i64 = e
        .db()
        .query_row(
            "SELECT COUNT(*) FROM deploys WHERE project='demo' AND target='prod'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "no deploy row should have been started");
}
