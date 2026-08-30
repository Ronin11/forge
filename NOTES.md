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
- **2026-08-30 · M6's process model is designed in before M1** (human decision:
  "fold M6 into the design now"). The daemon is the only SQLite opener; the worker
  is a detached child (or a systemd unit); the CLI is a thin client over a Unix
  socket with auto-start; `task` is the user-facing word. `forge run`/`forge
  serve`/`forge submit`/`forge answer`/`forge approve` never exist — the V2
  smoke steps are run with the M6 names (`docs/SMOKE.md`). M6 keeps only what
  M1–M5 do not need: drain-restart, `init`, `doctor`, `service`, capabilities,
  streaming (`logs -f`, `--wait`). Lease tolerance for a daemon restart is 120 s on
  both sides (daemon extends live leases on start; worker holds on through 120 s
  of failed heartbeats). Detached processes' raw stdio goes to
  `<component>.stdio.log`, structured logs to `<component>.log` — a deliberate
  split from the prompt's "stdio to worker.log" so the structured file never sees
  duplicate or unstructured lines. L3 sign-off gets `forge task approve|reject`,
  which the M6 tree lacked. `forge-m6-m7-prompt.md` stays the source for M7's
  plugin/indicator detail until `docs/PLUGINS.md` is written at M7 start.
- **2026-08-30 · Fold-in review decisions.** The CLI hands the daemon its locked
  `daemon.lock` descriptor as an extra file (no unlocked window); the daemon keeps
  lock and listener descriptors without `FD_CLOEXEC` across the drain `exec` (no
  socket gap); the daemon unlinks a stale socket itself after locking. A daemon
  start spawns a worker only if the worker data-dir lock is free; `daemon stop`
  stops the worker it spawned (`--keep-worker`). `--repo` resolves name → path →
  `<projects_root>/X` and registers unregistered checkouts on the fly by appending
  to `worker.toml`, which the worker re-reads every registration tick; the daemon
  re-reads `config.toml` only on restart (no SIGHUP mechanism). Lease extension on
  daemon start covers every claimed/preparing/running Target regardless of stored
  expiry. `complete` is idempotent by lease-token hash. `routine_name = "ad-hoc"`.
  Journal gains `daemon` and `plugin` entity types. Spawned processes get a fixed
  environment pass-through list (plugins included — a deliberate widening of
  "nothing else" so Python/Node plugins can start). `doctor` is one package used
  by the CLI (local checks, works with the daemon down) and `GET /api/v1/doctor`.
  `--wait` exits with the derived task state. systemd units use `KillMode=process`
  so the detached worker outlives a daemon restart. Schema-only handshake mismatch
  is still a mismatch, with a "to migrate" hint. Plain `daemon restart` (no drain)
  is M1 so M3 smoke 17 can restart; drain is M6.
- **2026-08-30 · M8–M12 folded into the design the same way as M6** (schema, state
  machine, config shapes, test harness; behaviour stays in its milestone). From M1:
  `fake-claude` is the test executor and `just check` runs offline (the M8 rule,
  adopted early so no test is ever rewritten); the merge-queue states are in the
  transition table (`succeeded` is terminal only when `integrate = false`, one home:
  `model.IsTerminal`); `attempt_facts` carries the integration/cost-vector/routing/
  human-loop columns as NULL; `routines.model` is an alias into `[models]` seeded
  with `haiku|sonnet|opus`; registries are `go generate`d and migrations
  ULID-named (STYLE §11, a deliberate exception to "no code generation"); Git
  options for worktrees go through `GIT_CONFIG_*` env only; the sandbox is one
  `Sandbox.Wrap` hook on the launch path. Constitution 9 (untrusted data) added now
  — the prompt says it needs no approval. The push-policy amendment waits for
  approval (proposed wording in the addendum report). Machine facts checked:
  `bwrap` 0.11.2 and `cargo` present, `mergiraf` absent, `rerere` on,
  `conflictstyle` unset, Claude config is `~/.claude`; **`strace` is not
  installed** (M8 write-path discovery needs it or an alternative) and unprivileged
  `unshare -n` is refused — the offline proof uses `unshare -Urn` or `bwrap
  --unshare-net`.
- **2026-08-30 · M8+ fold-in review decisions.** `integrate` is frozen on Work;
  `stack_on` lives on the dependency edge; `merging` is a leased merge claim run on
  a worker (sweeper retries, `max_rebase_attempts = 3` → `conflict`); `succeeded`
  is terminal only without integration (`model.IsTerminal`) and "success" is
  `model.IsSuccess`; facts are computed at attempt-terminal with a one-time fill of
  the integration columns (the sole immutability exception); worktrees awaiting
  merge are kept by their own cleanup row and the local task branch is
  fast-forwarded after a merge so the normal removal rule applies; `fake-claude`
  is a hidden subcommand behind the one template executor (fixtures hand-authored
  first, `meta.toml` schema fixed in DESIGN §7.4); `attempts.model` is the resolved
  id, `model_alias` the alias; `[runners]`/`[models]` are embedded defaults
  overridden per key; a per-attempt MCP token is minted at claim from M1 and `forge
  mcp` runs inside the sandbox behind a worker-side attempt-scoped socket (M8);
  the claim carries a `policy` block so the worker has no second config; the
  `deps` pre-step is a merge-queue commit, never a direct push; `forge task
  requeue` resolves `conflict`. `goimports` joins `fmt-check`; `just
  check-offline` is the STYLE §11 proof. `protocol` may import `model` (wire types
  carry model enums); `model` imports nothing.
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
