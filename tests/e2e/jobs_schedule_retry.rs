use crate::support::*;

#[test]
fn a_schedule_trigger_retries_once_after_one_failure() {
    let e = Env::new();
    let out = e.forge(
        "ok.sh",
        &[
            "project",
            "new",
            "shop",
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
    assert!(e.forge("ok.sh", &["workflows"]).status.success());
    let marker = e.home.join("failed-once");
    std::fs::write(
        e.home.join("workflows/scheduled-retry.toml"),
        format!(
            r#"name = "scheduled-retry"
kind = "run"
description = "fails its first assertion and succeeds on retry"
steps = [{{ action = "write-file", effect = "file" }}]
[trigger]
on = "schedule"
cron = "* * * * *"
[assert]
ok = ["bash", "-c", "if test -f '{}'; then exit 0; else touch '{}'; exit 1; fi"]
[limits]
budget_usd = 1.0
per_day = 10
on_failure = "retry:1"
"#,
            marker.display(),
            marker.display()
        ),
    )
    .unwrap();
    let input = e.home.join("input.json");
    std::fs::write(&input, r#"{"path":"out.txt","content":"hello"}"#).unwrap();
    let out = e.forge(
        "ok.sh",
        &[
            "job",
            "start",
            "shop",
            "scheduled-retry",
            "--input",
            input.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    // Seed the scheduler's queued row with a future slot so additional worker
    // passes cannot introduce unrelated firings at a minute boundary.
    let slot = "4102444800";
    e.db()
        .execute(
            "UPDATE jobs SET trigger_kind = 'schedule', trigger_ref = ?1 WHERE id = ?2",
            rusqlite::params![slot, id],
        )
        .unwrap();
    for _ in 0..3 {
        let out = e.forge("ok.sh", &["work", "--once"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let db = e.db();
    let mut stmt = db.prepare(
        "SELECT state, retry_count, trigger_kind, trigger_ref FROM jobs WHERE workflow = 'scheduled-retry' ORDER BY id",
    ).unwrap();
    let rows: Vec<(String, i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![
            ("failed".into(), 0, "schedule".into(), slot.into()),
            ("ok".into(), 1, "schedule".into(), slot.into()),
        ]
    );
    // Original firings still cannot duplicate this slot.
    assert!(
        db.execute("UPDATE jobs SET retry_count = 0 WHERE retry_count = 1", [])
            .is_err()
    );
}
