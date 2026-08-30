# Forge build notes

Running log of decisions, cuts, and open questions. Newest at the bottom.

## Decisions

- Module path is `forge`; std `flag` for the CLI (cobra not needed for seven subcommands).
- IDs are 32-hex random strings; "short id" is the first 8 characters. Branch names are
  `forge/<routine-slug>-<attempt8>`.
- Routine gets an `executor` field (default `claude-code`) even though only one executor
  ships, because the executor registry is the plugin boundary and the control plane must
  know which workers can run a Target.
- Concurrency is enforced at admission for both scheduled (skip) and manual (HTTP 409)
  runs, since the spec phrases it as a property of the Routine.
- Cleanup follows the spec's rule literally: a clean worktree with no unpushed commits is
  removed regardless of whether the attempt succeeded, failed, or was cancelled. Factory
  retained every non-successful worktree; Forge keeps the raw output file instead.
- Process identity is `/proc/<pid>/stat` start time (Linux only, which the spec allows).
- `robfig/cron` is used only for parsing; the scheduler computes `Next()` itself every
  10 s from a moving cursor, so missed runs are skipped for free.
- The worker's periodic registration doubles as its liveness heartbeat (30 s); a worker
  is "connected" if seen in the last 90 s. Per-attempt heartbeats (10 s) renew leases.
