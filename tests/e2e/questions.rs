use crate::support::*;

const DEFECT: &str = "`cargo test parse::edge` fails: left 3, right 4";
const ANSWER: &str = "keep the branch, make this one fix";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A task blocked on a review demotion at `blocked_at`, answered by `by`
/// two hours later, whose retry carries the answer and landed.
fn demoted_and_answered(e: &Env, blocked_at: i64, by: &str) -> i64 {
    let id = e.add(&[]);
    let retry = e.add(&[]);
    let db = e.db();
    db.execute(
        "UPDATE tasks SET state='blocked', reason=?2 WHERE id=?1",
        rusqlite::params![id, format!("review demoted: {DEFECT}")],
    )
    .unwrap();
    db.execute(
        "INSERT INTO attempts (task_id, attempt_no, step, state, reason, started_at, finished_at)
         VALUES (?1, 1, 'review', 'needs_input', ?2, ?3, ?3)",
        rusqlite::params![id, format!("review demoted: {DEFECT}"), blocked_at],
    )
    .unwrap();
    db.execute(
        "INSERT INTO decisions (task_id, repo, question, answer, created_at, answered_by, citations, retry_id)
         VALUES (?1, 'r', ?2, ?3, ?4, ?5, '', ?6)",
        rusqlite::params![id, DEFECT, ANSWER, blocked_at + 7200, by, retry],
    )
    .unwrap();
    db.execute(
        "UPDATE tasks SET retry_of=?2, state='succeeded', landed_sha='abc123',
                task = task || char(10) || char(10) || ?3
         WHERE id=?1",
        rusqlite::params![
            retry,
            id,
            format!("Supervisor's answer to a question from an earlier attempt (citing task 1): {ANSWER}")
        ],
    )
    .unwrap();
    id
}

fn questions(e: &Env, args: &[&str]) -> serde_json::Value {
    let mut a = vec!["stats", "--questions", "--json"];
    a.extend_from_slice(args);
    let o = e.forge("ok.sh", &a);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

#[test]
fn three_supervisor_answered_demotions_that_landed_are_do_it_as_stated() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(
        e.home.join("config.toml"),
        "[measure]\noperator_usd_per_hour = 120.0\n",
    )
    .unwrap();
    let t = now() - 36_000;
    for _ in 0..3 {
        demoted_and_answered(&e, t, "supervisor");
    }
    let old = demoted_and_answered(&e, 1000, "operator");
    assert!(old > 0);

    let doc = questions(&e, &["--days", "1"]);
    assert_eq!(doc["days"], 1);
    assert_eq!(doc["operator_usd_per_hour"], 120.0);
    let review = &doc["kinds"][0];
    assert_eq!(review["kind"], "review");
    assert_eq!(review["count"], 3, "{doc}");
    assert_eq!(review["answered_by_supervisor"], 3);
    assert_eq!(review["answered_by_operator"], 0);
    assert_eq!(review["withdrawn"], 0);
    assert_eq!(review["open"], 0);
    assert_eq!(review["as_stated"], 3);
    assert_eq!(review["median_wait_hours"], 2.0);
    assert_eq!(review["attention_cost_usd"], 0.0);
    assert_eq!(doc["kinds"].as_array().unwrap().len(), 5);
    assert_eq!(doc["total"]["count"], 3);

    let all = questions(&e, &[]);
    assert_eq!(all["total"]["count"], 4, "{all}");
    assert_eq!(all["total"]["answered_by_operator"], 1);
    let cost = all["total"]["attention_cost_usd"].as_f64().unwrap();
    assert!((cost - 5.0 / 60.0 * 120.0).abs() < 1e-9, "{cost}");

    let o = e.forge("ok.sh", &["stats", "--questions"]);
    assert!(o.status.success());
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(
        text.contains("review") && text.contains("ASSTATED"),
        "{text}"
    );
}
