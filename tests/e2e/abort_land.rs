//! A stop revokes publication: an abort recorded while the checks run is
//! honoured at the land step and at the push, never after the base moved
//! (docs/CLIENT.md, `forge withdraw --abort`).

use crate::support::*;
use std::time::Duration;

#[test]
fn an_abort_during_verification_lands_and_pushes_nothing() {
    let e = Env::new();
    // The task check runs sandboxed after the agent; it waits for a file
    // the test writes once the abort is recorded, so the window needs no
    // clock.
    let check = "for i in $(seq 1 180); do [ -f go ] && exit 0; sleep 1; done; exit 1".to_string();
    // 1 is a failed parent; 2 and 3 retry it, so each is the other's live
    // sibling, which is what `--abort` requires.
    let parent = e.add(&[]);
    e.db()
        .execute(
            "UPDATE tasks SET state='failed', reason='L1 failed: answer' WHERE id=?1",
            [parent],
        )
        .unwrap();
    let doomed = e.add(&["--check", &check]);
    let sibling = e.add(&[]);
    e.db()
        .execute(
            "UPDATE tasks SET retry_of=?1 WHERE id IN (?2, ?3)",
            [parent, doomed, sibling],
        )
        .unwrap();
    let log = e.home.join("worker.log");
    let out = std::fs::File::create(&log).unwrap();
    let mut cmd = e.cmd("ok.sh");
    // Unsandboxed, so the check sees the gate file written into its
    // worktree from outside; the abort's path does not depend on the
    // sandbox.
    cmd.env("FORGE_SANDBOX", "0");
    cmd.args(["work", "--once"])
        .stdout(out.try_clone().unwrap())
        .stderr(out);
    let mut worker = Worker::spawn(&mut cmd);
    let read = || std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        wait_until(
            || read().contains("check L2/task-check-1"),
            Duration::from_secs(180)
        ),
        "the task check never started: {}",
        read()
    );
    // The agent is done and the checks are running: stop it now.
    let o = e.forge(
        "ok.sh",
        &[
            "withdraw",
            &doomed.to_string(),
            "--abort",
            "--reason",
            "a duplicate",
        ],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // The checks run in the task's worktree, refreshed from the kernel
    // repository for verification.
    let gate = e.home.join("worktrees").join(doomed.to_string()).join("go");
    std::fs::write(&gate, "")
        .unwrap_or_else(|e| panic!("{e} writing {}:\n{}", gate.display(), read()));
    // The worker's exit reflects the capped task; it ends either way.
    let _ = worker.wait();

    let (state, reason, pushed) = e.task(doomed);
    assert_eq!(state, "capped", "{reason}\n{}", read());
    assert!(reason.starts_with("aborted: a duplicate"), "{reason}");
    assert!(!pushed, "a stop revokes publication: {}", read());
    assert!(
        read().contains("CAPPED") && read().contains("aborted: a duplicate"),
        "{}",
        read()
    );
    assert!(!read().contains("[2] pushed"), "{}", read());
    // The sibling carries the work.
    let (state, reason, _) = e.task(sibling);
    assert_eq!(state, "succeeded", "{reason}");
    assert!(reason.starts_with("landed main @"), "{reason}");
}
