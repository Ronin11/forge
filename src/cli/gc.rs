use super::*;

/// What the worker does hourly (`housekeeping::run`), aloud. Nothing with
/// work is deleted: a finished task's tree is kept `worktree_retention_days`
/// (or `older_than`), then its leftovers are committed, its branch is
/// published (to the remote when the repo has one, else into the registered
/// repository) and the tree is removed. Besides that, a worktree goes when
/// it is clean and every commit it added is already reachable from a remote
/// ref (or it added none). Everything else is kept with the reason and the
/// command a human would run. Branches are never deleted, and a pushed
/// branch lands without its worktree (`forge land` recreates it), so
/// removing one loses nothing.
pub(super) async fn gc(dry_run: bool, older_than: Option<i64>, caches: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    if caches {
        let bytes = crate::disk::sweep(&f.store, &f.paths.worktrees, dry_run)?;
        out!(
            "{} {bytes} bytes of build caches",
            if dry_run { "would free" } else { "freed" }
        );
        return Ok(());
    }
    let pass = crate::housekeeping::Pass {
        dry_run,
        manual: true,
        older_than,
        retention_days: f.worker.worktree_retention_days,
        now: crate::unix_now(),
    };
    let report = crate::housekeeping::run(&f, &pass).await?;
    for line in &report.lines {
        out!("{line}");
    }
    // The last line is the verdict gc-nightly records as its effect: the
    // counts and the bytes, so the record says what a night freed and
    // what it left (docs/OPS.md, "What doctor watches").
    out!(
        "{} {}, kept {}; {} bytes {}, {} bytes kept",
        if dry_run { "would remove" } else { "removed" },
        report.removed,
        report.kept,
        report.freed,
        if dry_run { "would be freed" } else { "freed" },
        report.kept_bytes
    );
    Ok(())
}
