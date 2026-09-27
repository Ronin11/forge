use crate::support::*;

#[test]
fn two_concurrent_forge_land_calls_land_the_task_exactly_once() {
    // REVIEW-3 2.1#7: `land_task` used to check `landed_sha` and recreate
    // the worktree before `integrate` took the repository lock, so a
    // second `forge land` (or the supervisor's accept-and-land) racing the
    // first landed the same task twice. `land_task` now takes the lock
    // before it reads the task, so the loser blocks on it and, once it
    // gets it, sees `landed_sha` already set.
    let e = Env::new();
    assert!(e.run("ok.sh", &["--retries", "0"]).status.success());
    let (state, _, _) = e.task(1);
    assert_eq!(state, "succeeded");

    let mut c1 = e.cmd("ok.sh");
    c1.args(["land", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut c2 = e.cmd("ok.sh");
    c2.args(["land", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child1 = c1.spawn().unwrap();
    let child2 = c2.spawn().unwrap();
    let outputs = [
        child1.wait_with_output().unwrap(),
        child2.wait_with_output().unwrap(),
    ];

    let landed: Vec<_> = outputs.iter().filter(|o| o.status.success()).collect();
    let refused: Vec<_> = outputs.iter().filter(|o| !o.status.success()).collect();
    assert_eq!(
        landed.len(),
        1,
        "exactly one call lands the task: {outputs:?}"
    );
    assert_eq!(refused.len(), 1, "the other is refused: {outputs:?}");
    assert!(
        String::from_utf8_lossy(&landed[0].stdout).contains("landed task 1 on main @ "),
        "{:?}",
        landed[0]
    );
    assert!(
        String::from_utf8_lossy(&refused[0].stderr).contains("already landed"),
        "{:?}",
        refused[0]
    );

    let (state, reason, pushed) = e.task(1);
    assert_eq!(state, "succeeded");
    assert!(reason.starts_with("landed main @ "), "{reason}");
    assert!(pushed);
    assert_eq!(
        op_names(&e, 1).iter().filter(|(n, _)| n == "land").count(),
        1,
        "one land op row: {:?}",
        op_names(&e, 1)
    );
}
