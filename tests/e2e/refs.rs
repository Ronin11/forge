use crate::support::*;

#[test]
fn refs_are_recorded_and_carried_by_ref_list_trace_and_show() {
    let e = Env::new();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    let (state, _, _) = e.task(1);
    assert_eq!(state, "succeeded");

    let o = e.forge(
        "ok.sh",
        &[
            "ref",
            "add",
            "1",
            "--kind",
            "pr",
            "--url",
            "https://example.com/pulls/42",
            "--label",
            "fix thing",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let o = e.forge(
        "ok.sh",
        &[
            "ref",
            "add",
            "1",
            "--kind",
            "issue",
            "--url",
            "https://example.com/issues/7",
            "--by",
            "notify",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // A task that doesn't exist is refused.
    let bad = e.forge(
        "ok.sh",
        &["ref", "add", "999", "--kind", "pr", "--url", "https://x"],
    );
    assert!(!bad.status.success());

    // `forge ref list --json` carries both, in order, with the right kind/url/label/by.
    let rows: serde_json::Value =
        serde_json::from_slice(&e.forge("ok.sh", &["ref", "list", "1", "--json"]).stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["task_id"], 1);
    assert_eq!(rows[0]["kind"], "pr");
    assert_eq!(rows[0]["url"], "https://example.com/pulls/42");
    assert_eq!(rows[0]["label"], "fix thing");
    assert_eq!(rows[0]["by"], "operator");
    assert_eq!(rows[1]["kind"], "issue");
    assert_eq!(rows[1]["url"], "https://example.com/issues/7");
    assert_eq!(rows[1]["label"], "");
    assert_eq!(rows[1]["by"], "notify");

    // `forge trace --json` carries the same two references on the task.
    let doc = e.trace_json(1);
    let refs = doc["task"]["refs"].as_array().unwrap();
    assert_eq!(refs.len(), 2, "{refs:?}");
    assert_eq!(refs[0]["kind"], "pr");
    assert_eq!(refs[0]["url"], "https://example.com/pulls/42");
    assert_eq!(refs[1]["kind"], "issue");
    assert_eq!(refs[1]["url"], "https://example.com/issues/7");

    // `forge show` prints both, as `ref` lines under the lineage.
    let out = String::from_utf8_lossy(&e.forge("ok.sh", &["show", "1"]).stdout).to_string();
    let workflow_at = out.find("workflow   ").unwrap_or_else(|| panic!("{out}"));
    let pr_at = out
        .find("ref        pr https://example.com/pulls/42 (fix thing)")
        .unwrap_or_else(|| panic!("{out}"));
    let issue_at = out
        .find("ref        issue https://example.com/issues/7")
        .unwrap_or_else(|| panic!("{out}"));
    assert!(pr_at < issue_at, "{out}");
    assert!(issue_at < workflow_at, "{out}");
}
