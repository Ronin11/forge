use crate::support::*;

fn initiative(e: &Env) -> i64 {
    let out = e.forge(
        "ok.sh",
        &[
            "project",
            "new",
            "demo",
            "--purpose",
            "p",
            "--repo",
            e.repo.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = e.forge(
        "ok.sh",
        &["initiative", "new", "demo", "--outcome", "finish"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("created initiative "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn settled(e: &Env, id: i64) -> bool {
    e.db()
        .query_row(
            "SELECT settled_at IS NOT NULL FROM initiatives WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn invalid_base_config_fails_the_task_emits_done_and_settles_its_initiative() {
    let e = Env::new();
    let iid = initiative(&e);
    let id = e.add(&["--initiative", &iid.to_string(), "--retries", "0"]);
    std::fs::write(e.repo.join("forge.toml"), "[invalid").unwrap();
    git(&e.repo, &["add", "forge.toml"]);
    git(&e.repo, &["commit", "-m", "invalid base configuration"]);
    git(&e.repo, &["push", "origin", "main"]);
    let out = e.forge("neverrun.sh", &["work", "--once"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (state, reason, _) = e.task(id);
    assert_eq!(state, "failed");
    assert!(reason.contains("error:"), "{reason}");
    assert!(settled(&e, iid));
    let events = std::fs::read_to_string(e.home.join("events.jsonl")).unwrap();
    assert!(
        events
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|event| event["type"] == "task_done" && event["state"] == "failed"),
        "{events}"
    );
}

#[test]
fn claim_backstop_settles_an_initiative_after_an_interrupted_terminal_tail() {
    let e = Env::new();
    let iid = initiative(&e);
    let id = e.add(&["--initiative", &iid.to_string()]);
    e.db()
        .execute(
            "UPDATE tasks SET state='failed', finished_at=1 WHERE id=?1",
            [id],
        )
        .unwrap();
    assert!(!settled(&e, iid));
    assert!(e.forge("neverrun.sh", &["work", "--once"]).status.success());
    assert!(settled(&e, iid));
}
