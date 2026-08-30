# Forge build notes

Running log of decisions, cuts, review findings not fixed, and open questions.
Newest entries at the bottom of each section. Dates are absolute.

## Decisions

- **2026-08-30 · V2 starts on top of V1 history.** The repo carried five V1 commits
  whose files had been deleted from the working tree. The deletions were committed
  (`chore: clear V1 tree`) rather than rewriting history; V1 stays readable with
  `git show 0148b45:…` for reference. Nothing from V1 is reused without re-reading it
  against `docs/STYLE.md`.
- **Module path is `forge`.** Not published; a dotted path would be ceremony.
- **Std `flag`, one file per subcommand.** Sixteen subcommands do not justify cobra.
- **IDs are 32-hex; short IDs 8; branch `forge/<slug>-<attempt8>`; worktrees are a
  flat `<data_dir>/worktrees/<attempt-id>`.** Flat paths make the manifest's
  "worktree path must equal the owned path" check a string equality.
- **Fetch is best-effort** (`DESIGN.md` §5.2). Factory required the remote's truth
  because it ran a fleet; Forge runs on one laptop that is often offline. Every
  attempt records whether the fetch succeeded, so an analysis can exclude stale bases.
- **Process identity is `/proc/<pid>/stat` start time**, Linux only. Factory used
  `ps -o lstart= -o command=`; the spec fixes the platform to Arch Linux so the
  simpler exact check wins. A second platform is one file.
- **One Attempt per Target across resumes.** A `waiting_human` pause keeps the same
  attempt, worktree, branch, and session; `launches` counts processes; elapsed time
  carries over through the manifest so `elapsed_us` is monotone and excludes waiting.
- **Claim is admission.** The budget decision is evaluated at claim time by one pure
  function; there is no separate "admitted" state to keep consistent.
- **Burn-down line is `target × fraction-of-window-elapsed`.** Backlog admits when
  utilisation is below the line; the line rises to the target at reset. Simple to
  explain, one formula, no tuning knobs beyond the targets.
- **`forge mcp` is a pure HTTP client of the control plane.** One SQLite writer;
  tool spans are recorded where the tool runs.
- **Reconcile does not call `complete` for a Target the control plane already
  closed** — it patches cleanup fields. A late `complete` for an open Target is a
  normal completion with `worker_restart` as the reason.
- **2026-08-30 · Review-gate decisions (M0).** `verifying` holds no lease; the sweeper
  covers only `claimed/preparing/running`. The heartbeat runs from claim until
  `complete` returns because L1 checks outlast a 30 s lease. Verify Work has no
  routine, so a `concurrency = 1` routine can still be verified. Events carry a
  `source` in their primary key so worker, `forge mcp`, and control-plane spans never
  collide, and the manifest carries `next_seq` so a resume never collides with the
  first launch. Repository tools (`forge_check`, `forge_repo_status`,
  `forge_diff_summary`) run inside `forge mcp` (a child of the agent, in the
  worktree, in its process group) rather than in the control plane. `forge_ask` is
  non-blocking; "open Question at exit ⇒ waiting_human" is the single rule.
  Auto-revert only ever restores the last human-approved generation and is part of
  the approved A/B plan — that is how it squares with constitution 5. `forge cleanup`
  is a worker-local command that takes the cross-process per-repository flock
  (`<data_dir>/locks/<repo>.lock`, shared with the running worker); `retained`
  manifests are final so the worker never races it. `forge prune` deletes rows via
  the API and files via `worker.toml` (both configs are on the one machine).
- **2026-08-30 · Logging standard and journal added before any store code.** One
  handler, per-component levels by longest dotted prefix, correlation attrs in
  context, stderr governed by flags, file sink fixed at debug JSON with size
  rotation written in-package (~80 lines; a dependency was not worth a reason).
  `-v`/`-vv` override the default level but keep `--log-level` component
  overrides. SIGUSR1 toggles debug and remembers the previous level; an explicit
  set clears that memory. The journal table is *specified* (`DESIGN.md` §3, `STYLE.md`
  §9) to be in migration 1 and written in the same transaction as every
  Work/Target/Attempt/Question/Proposal state change; the store lands in M1. Logs
  are never authoritative. `OpenFileSink` takes no context (STYLE §3 carve-out for
  non-blocking syscalls). Detached processes get `<component>.stdio.log` for raw
  stdio so the structured file never receives duplicate lines. The daemon/plugin work discussed
  separately (a `daemon` subcommand, plugin loading) is the M6/M7 delta; only the
  vocabulary (`daemon`, `plugin` component) is used here.
- **Third-party modules** (why): `modernc.org/sqlite` — SQLite without cgo so the
  binary builds anywhere Go does; `BurntSushi/toml` — the config format the spec
  fixes; `robfig/cron/v3` — cron parsing only, `Next()` is computed by Forge;
  `mark3labs/mcp-go` — the MCP wire protocol, evaluated in M2 (fallback: hand-written
  stdio JSON-RPC); `golang.org/x/sync` — errgroup. Added when first used, not before.

## Cuts and deferrals

- **M0:** no `bench/threshold.txt` and no `scripts/bench-check.sh` until the first
  benchmark exists (M1); `just bench`, `kb-check`, and `ui-test` print "not yet"
  until their milestones so `just check` is honest about what it covers.

## Review findings not fixed

(none yet)

## Open questions

- See the M0 report; resolved answers are moved into Decisions with the date.
