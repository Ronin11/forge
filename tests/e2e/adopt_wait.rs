use crate::support::*;
use std::time::Duration;

fn state_if_any(e: &Env, id: i64) -> String {
    e.db()
        .query_row("SELECT state FROM tasks WHERE id=?1", [id], |r| r.get(0))
        .unwrap_or_default()
}

#[test]
fn an_adopt_killed_while_it_waits_for_a_slot_ends_blocked_with_a_retry_hint() {
    let e = Env::new();
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(e.home.join("config.toml"), "[worker]\nslots = 1\n").unwrap();
    let toml = std::fs::read_to_string(e.repo.join("forge.toml")).unwrap();
    std::fs::write(
        e.repo.join("forge.toml"),
        format!(
            "{toml}gate = [\"bash\", \"-c\", \"for _ in $(seq 6000); do [ -e .git/gate-open ] && exit 0; sleep 0.1; done; exit 1\"]\n"
        ),
    )
    .unwrap();
    git(&e.repo, &["commit", "-qam", "a gated check"]);
    git(&e.repo, &["push", "-q", "origin", "main"]);
    git(&e.repo, &["checkout", "-q", "-b", "by-hand", "main"]);
    std::fs::write(e.repo.join("answer.txt"), "42\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "-qm", "by hand"]);
    git(&e.repo, &["checkout", "-q", "main"]);
    let busy = e.add(&["--retries", "0", "--no-land"]);
    let worker = Worker::spawn(e.cmd("ok.sh").args(["work", "--poll", "1"]));
    assert!(wait_until(
        || state_if_any(&e, busy) == "running",
        Duration::from_secs(60)
    ));
    let mut adopt = e
        .cmd("neverrun.sh")
        .args(["adopt", e.repo.to_str().unwrap(), "by-hand", "--no-land"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let adopted = busy + 1;
    assert!(wait_until(
        || state_if_any(&e, adopted) == "queued",
        Duration::from_secs(60)
    ));
    adopt.kill().unwrap();
    adopt.wait().unwrap();
    assert!(
        wait_until(
            || state_if_any(&e, adopted) == "blocked",
            Duration::from_secs(60)
        ),
        "an abandoned adoption must not stay queued"
    );
    let reason: String = e
        .db()
        .query_row("SELECT reason FROM tasks WHERE id=?1", [adopted], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        reason.contains(&format!("forge retry {adopted}")),
        "{reason}"
    );
    open_gate(&e, busy);
    drop(worker);
}
