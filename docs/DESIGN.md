# Forge design

Forge runs coding agents against local Git checkouts, measures everything they do, and
uses those measurements to improve its own prompts, tools, docs, and processes. One Go
binary, one SQLite file, one machine, one developer, one subscription-authenticated
Claude Code CLI.

It is a clean-room rewrite of Factory (`561e883`). Factory's execution machinery —
worktree isolation, durable manifests, leases, process-group supervision, fail-closed
cleanup — encodes bugs already found and fixed; those invariants are carried over and
named below. Factory's fleet features (remote workers, managed clones, Codex/Pi
runtimes, React UI) are not.

Companion documents: `CONSTITUTION.md` (fixed principles), `STYLE.md` (code standard),
`MODES.md` (each mode's prompt, tools, schema, checkpoints), `VERIFICATION.md`
(levels L0–L3 and how each is decided), `MODULARIZATION.md` (the planned split of the
single module into core / tools / web / tui, and the dependency direction it fixes).

## 1. Shape

```
  forge <cmd> (thin CLI)   browser         plugins (M7)
        │ unix socket        │ 127.0.0.1:7340   │ unix socket + scoped token
        ▼                    ▼                  ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ forge daemon — the only process that opens SQLite                   │
  │  http api (one mux, two listeners) · scheduler · budget · queue     │
  │  stats · ui · plugin supervision · kb index                         │
  │  SQLite (<forge home>/forge.sqlite3, WAL) · journal                 │
  └───────────▲──────────────────────────────────▲───────────────────────┘
              │ register / claim / heartbeat      │ tool calls (http)
              │ events / complete                 │
  ┌───────────┴───────────────┐        ┌──────────┴─────────────────────┐
  │ forge worker (detached    │        │ forge mcp --attempt <id>       │
  │ child of the daemon, or   │        │ stdio MCP server, one per      │
  │ a systemd unit)           │──exec─▶│ agent process; http client of  │
  │ repos · git · worktrees   │        │ the daemon; runs repo tools    │
  │ manifests · supervisor    │        │ in-process                     │
  │ parser · reconcile        │        └────────────────────────────────┘
  └───────────┬───────────────┘
              │ exec, Setpgid
      claude --print … (cwd = worktree)
```

**Every `forge` command is a thin client.** One daemon owns the state; the CLI,
worker, MCP server, and plugins reach it only through the HTTP+JSON API. If the
daemon is not running, a command starts it (§1.2) and proceeds. `forge task add "…"
--repo X` on a fresh machine works with no other setup (§1.3).

### 1.1 Process model

- **`forge daemon`** is the control plane, scheduler, budget policy, UI, kb index,
  and plugin supervisor in one long-lived process, and the only process that opens
  SQLite. It holds `flock(<home>/daemon.lock)` for its lifetime (the descriptor is
  kept without `FD_CLOEXEC` so the lock survives the `exec` of §1.4) and writes
  `<home>/daemon.json` (`pid`, `pid_start` from `/proc/<pid>/stat`, `worker_pid`,
  `version`, `schema_version`, `socket`, `http`, `started_at`, `state:
  running|draining`) atomically (tmp + rename). Liveness is never inferred from
  `daemon.json` alone: `daemon status` and auto-start trust only "the lock is held"
  (a non-blocking `flock` attempt, released on success) and `pid`+`pid_start`; a
  file left by a `kill -9` is reported as stale. After taking the lock and before
  `bind`, the daemon unlinks a leftover `forge.sock` itself (the lock proves no
  other daemon owns it) and creates the new one under `umask 077`. It never
  idle-exits. `forge daemon start` runs it detached (`--foreground` to stay
  attached); `stop` sends SIGTERM and waits (`--force` SIGKILLs and leaves reconcile
  to clean up); `restart` drains (§1.4); `status`, `logs [-f]`, `log-level LEVEL`.
- **The worker is a separate process.** On start the daemon ensures a worker is
  running: if the daemon itself runs under systemd (`INVOCATION_ID` set) it runs
  `systemctl --user start forge-worker`; otherwise, if the worker data-directory
  lock (`<home>/worker/lock`, §7.1) is free, it spawns `forge worker` as a detached
  child (`setsid`, own process group, stdio to `<home>/logs/worker.stdio.log`,
  environment reduced to the pass-through list below) and records `worker_pid`; if
  the lock is held a worker from before the restart is still alive and is left
  alone. The worker survives a daemon restart: it keeps executing, retries
  heartbeats with backoff, reconnects, and re-registers immediately on the first
  successful request after a failure (§14). `forge daemon stop` also stops the
  worker it spawned (`--keep-worker` leaves it); a systemd-owned worker is
  systemd's. `forge worker start` runs it by hand or under systemd.
- **Environment pass-through** for every process Forge spawns (daemon, worker,
  executor, plugin): exactly `PATH`, `HOME`, `USER`, `LANG`, `LC_*`, `TERM`,
  `XDG_*`, `SSH_AUTH_SOCK`, `CLAUDE_CONFIG_DIR`, `ANTHROPIC_*`, `FORGE_HOME`,
  `FORGE_HTTP`, `FORGE_LOG_*`, plus what the spawner sets itself (`FORGE_LOG_*`
  from `logging.Environ`; `FORGE_SOCKET`/`FORGE_TOKEN`/`FORGE_PLUGIN_DIR` for
  plugins; `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` for
  executors and git; `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY` for sandboxed
  executors). The worker token is never passed to an executor. Nothing else leaks
  from the parent.
- **Transport.** The same API on two listeners: a Unix socket `<home>/forge.sock`
  (mode 0600, directory 0700; filesystem permissions are the auth) for the CLI,
  worker, MCP server, and plugins; and `127.0.0.1:7340` for the browser. One mux,
  one handler set. Streaming endpoints (`task logs -f`, `--wait`, the journal) use
  SSE with event ids and a `?since=` cursor so a client resumes after any restart.
  **Auth:** over the socket a client without a token is the operator (the peer is
  this uid); a client presenting a token is narrowed to it — a plugin token to its
  scopes (§17), a per-attempt MCP token (minted at claim, `attempts.mcp_token_hash`)
  to that attempt's routes (`/attempts/{id}/*`, `/tools/*` with that attempt id).
  Tokens are a discipline on the same uid, not a security boundary — except inside
  the sandbox (§19), which sees no daemon socket at all, only a worker-side
  per-attempt socket that forwards attempt-scoped routes. Over TCP, operator routes
  are open to loopback; `/api/v1/worker/*`, `/attempts/*` (worker side), and
  `/tools/*` require the worker token (`<home>/token`) or a plugin token.
  `[http] listen` in `config.toml` (default `127.0.0.1:7340`; env `FORGE_HTTP`)
  sets the TCP address; a bind failure is fatal with a one-line error naming the
  port.
- **`FORGE_HOME`** overrides `~/.forge` for every process (the CLI passes it to the
  daemon and worker it spawns, and puts it in the per-attempt MCP config's `env`
  together with `FORGE_SOCKET`, so `forge mcp` never depends on `claude` forwarding
  the worker's environment); it exists for the fresh-box smoke test and for running
  two Forges side by side.

### 1.2 Auto-start (each step guards a real race)

1. Connect to the socket, retrying for up to 2 s (a daemon mid-`exec`, §1.4, is
   not down). On success, `GET /api/v1/handshake`, then proceed.
2. If `~/.config/systemd/user/forge.service` exists **and** `FORGE_HOME` is unset or
   is `~/.forge` (a unit serves only the default home), run `systemctl --user start
   forge`; if that fails, print its stderr and exit non-zero; otherwise wait for
   the socket (≤ 10 s) and never spawn a daemon yourself.
3. Otherwise take `<home>/daemon.lock` with a non-blocking `flock`. If it is held,
   someone else (a live daemon, or another CLI starting one) owns it: wait for the
   socket ≤ 10 s, then fail with `daemon pid N is running but <home>/forge.sock is
   missing — run 'forge daemon restart'` (or, if `daemon.json` names a dead pid,
   with a hint to remove the stale lock).
4. Holding the lock, remove a stale socket file and spawn `forge daemon` detached
   (`setsid`, stdio to `<home>/logs/daemon.stdio.log`, the pass-through
   environment) **passing the locked descriptor as an extra file**: `flock` belongs
   to the open-file description, so the child inherits the lock with no window in
   which nobody holds it; the daemon keeps that descriptor. The CLI closes its copy
   once `daemon.json` shows the new pid (≤ 5 s; on timeout it closes it anyway,
   prints the tail of `daemon.stdio.log`, and exits non-zero), then polls the
   socket (≤ 5 s) and proceeds.
5. **Handshake** returns `version` and `schema_version`. On mismatch of either the
   CLI prints one line — `forge: daemon is v0.3.1, this CLI is v0.4.0 — run 'forge
   daemon restart'` (for a schema-only mismatch: `— run 'forge daemon restart' to
   migrate`) — and exits non-zero. It never auto-restarts on mismatch. The commands
   that *are* the cure — `daemon restart|stop|status|logs`, `doctor`, `version` —
   print the line as a warning and continue.

Commands that never auto-start the daemon: `daemon start|stop|status|logs`,
`service *`, `worker start`, `mcp`, `version`, `doctor` (which inspects the
database by `stat()` only and never opens SQLite), and `cleanup`/`prune`, which
read `worker.toml` and touch worker files locally and call the API only if the
daemon is already up.

### 1.3 Bootstrap vs. `init`

- **Implicit bootstrap** runs on every daemon start, idempotently, without prompts
  or network: create `<home>` (0700) and its subdirectories (`logs`, `logs/plugins`,
  `worker`, `kb`, `modes`, `plugins`, `deps`), the worker token, the schema
  (migrations), the `default` project, seed `modes/*.md` and the default
  `<home>/config.toml` (daemon: `[http]`, `[budget]`, `[kb]`, `[log]`) and
  `<home>/worker.toml` (only if absent). The Forge repository is registered in the
  seeded `worker.toml` only when the running binary sits inside a Git checkout
  (`os.Executable()` and its parents); otherwise it is skipped and `init` offers it.
- **Repositories on the fly.** `--repo X` resolves in order: a registered name; a
  path (absolute, or relative to the cwd); `<projects_root>/X` (`[repositories]
  projects_root` in `config.toml`, default `~/Projects`). An unregistered checkout
  with an `origin` is registered on the fly under its directory name: the daemon
  appends the entry to `worker.toml` (the daemon owns every file bootstrap writes)
  and the worker re-reads `worker.toml` on its next registration tick (≤ 30 s) and
  advertises it. This is what makes `forge task add "say hi" --repo equitizr` work
  on a fresh box. `init` writes the same files; the daemon re-reads `config.toml`
  only on `daemon restart`.
- **`forge init`** is the optional interactive half (`--yes` takes defaults):
  detect `claude`, `gh`, `git`, `node`/`npx` and report versions; offer to register
  repositories (Git checkouts with an `origin` under `~/Projects`, plus the Forge
  repo, pre-checked); choose the kb path; `--service` installs the systemd user
  units (`forge.service`, `forge-worker.service`); `--with-browser` installs
  Playwright under `<home>/deps/` (never global). It prints what it did and skipped.
- **`forge doctor`** re-runs every detection — binaries, auth, socket, daemon
  version, DB permissions, disk space, stale locks, orphaned worktrees, plugin
  health — as a table with a fix hint per red row; exit non-zero if anything is red.
  One `doctor` package holds every check; the CLI runs the local ones (binaries,
  socket, lock, permissions, disk) itself so it works with the daemon down, and asks
  `GET /api/v1/doctor` for the rest when the daemon is reachable.
- **systemd units** (`forge service install`): `forge.service` — `Type=simple`,
  `ExecStart=<abs forge> daemon start --foreground`, `KillMode=process` (the
  detached worker is not the unit's to kill), `Restart=on-failure`,
  `Environment=FORGE_HOME=<home>`; `forge-worker.service` — same shape with
  `worker start`, `After=forge.service`. `install` enables both; `uninstall`
  disables and removes them. Linger is never enabled by Forge; `doctor` mentions
  `loginctl enable-linger` when the units exist and linger is off.

### 1.4 Restart with drain

`forge daemon restart` sends `POST /api/v1/daemon/drain {exec, timeout}` where
`exec` is the CLI's own `os.Executable()` (the daemon's `/proc/self/exe` may read
`(deleted)` after an upgrade); the daemon checks it is a regular executable file.
Draining: stop admitting (`claim` and operator writes answer 503 — a 5xx, so the
worker keeps retrying rather than disabling sends, §8); keep serving worker
heartbeats, events, and completions; close SSE streams with a `retry` hint (clients
resume from their cursor); set `state: draining` in `daemon.json` and the UI; journal
`daemon.draining`; wait up to `timeout` (default 30 s) for remaining in-flight
requests; then `exec` the new binary with `FORGE_HOME`, `logging.Environ`, the lock
descriptor, and **both listener descriptors** inherited — no socket gap, same pid,
same lock. The new process journals `daemon.restarted`. Running attempts continue on
the worker throughout.

`forge run` and `forge serve` do not exist; the one long-lived operator-facing
process is the daemon.

Trust model: one operator, one machine. Worktrees isolate Git state, not hostile code.

## 2. Identifiers and naming

One home: `internal/model/id.go`.

| Thing | Format | Example |
|---|---|---|
| ID (every table) | 32 lower-case hex from `crypto/rand` | `3f9a…` |
| Short ID | first 8 characters | `3f9a1c2e` |
| Branch | `forge/<routine-slug>-<attempt8>` | `forge/inventory-3f9a1c2e` |
| Worktree path | `<data_dir>/worktrees/<attempt-id>` | |
| Manifest | `<data_dir>/attempts/<attempt-id>.json` (0600) | |
| Raw output | `<data_dir>/output/<attempt-id>.log` | |
| Artifacts | `<data_dir>/artifacts/<attempt-id>/` | |
| MCP config | `<data_dir>/mcp/<attempt-id>.json` | |
| Name (routine, repository, project, worker) | `^[a-z0-9][a-z0-9-]{0,39}$`, validated in `model.ValidateName`; names are already slugs | `weekly-audit` |
| Span ID | unique within the attempt: the phase name for phase spans, `agent-<launch>` for each agent launch, `hex(sha256(tool_use_id))[:8]` for tool spans, `mcp-<n>` for Forge tool calls; the root span is the attempt short ID | |
| Fact link | `attempt:<id>` `work:<id>` `target:<id>` `routine:<name>[@<gen>]` `repository:<name>` `project:<name>` `proposal:<id>` `prompt:<hash>` | |

IDs become paths and branch names, so every ID received over the wire is validated
against `^[0-9a-f]{32}$` before use. Names become branch slugs, URL path segments,
TOML keys, and fact links, so they are validated against the pattern above wherever
they enter (config load, routine create, registration).

## 3. Data model

All tables exist from the first migration, even those unused until later milestones.
Every table has `id TEXT PRIMARY KEY`, `created_at` (UTC RFC 3339), and, where rows
change, `updated_at`. Foreign keys are declared and enforced.

### Repository (worker-owned, advertised)

From `[repositories.<name>]` in `~/.forge/worker.toml`: `path` (non-bare checkout
with an `origin`), optional `base_branch`, optional `project` (default `default`).
The worker validates each on start (§7.1), pins the normalised origin identity, and
advertises `{name, path, origin_identity, base_branch, project}` on every
registration. The control plane upserts `repositories(name, project_id, path,
origin_identity, base_branch, worker_id, last_seen_at)`; it never clones.

Each repository Forge works on has a **`.forge/` directory** — the repo-scoped
home (decided with M4): `.forge/config.toml` is the preferred location of the
configuration below (a top-level `forge.toml` is still honoured, `.forge/`
wins); `.forge/modes/<mode>.md` are per-repo prompt overlays appended after the
global mode preamble and hashed into the prompt version; `.forge/notes/` holds
repo-scoped kb notes in the standard format, indexed by the daemon alongside
the global kb (ids must be globally unique; `forge kb check` reports
collisions as parse findings). Overlays and notes are read from the registered
checkout (read-only) and from worktrees like any repository content.

The configuration (either location) declares:

```toml
[checks]                      # each value is one command as argv; run in the worktree
build = ["go", "build", "./..."]
test  = ["go", "test", "./..."]
lint  = ["just", "lint"]

[defaults]
autonomy = "checkpoint"       # overrides the project's level for this repository
base_branch = "main"

[modes.docs]
paths = ["docs/**", "*.md"]   # what the docs mode may touch
```

The worker reads it from the worktree at attempt start and sends it with the
`preparing` heartbeat so the control plane records the checks the attempt will be
verified against.

### Project

`projects(id, name UNIQUE, autonomy, budget_class, priority_baseline)`. Every
repository belongs to exactly one project; `default` is created by bootstrap.

### Routine

`routines(id, name UNIQUE, mode, prompt, repositories JSON, executor, model, effort,
max_turns, timeout_seconds, max_budget_usd, allowed_tools JSON, autonomy, verification,
priority, budget_class, schedule, schedule_enabled, concurrency, paths JSON, deps JSON,
tier, models JSON, integrate, require_sandbox, max_questions, generation,
archived_at)`. The last seven are the M9–M11 columns (§20–§22): `paths` are the
write-set globs, `deps` the dependencies a pre-step adds, `tier` (0–3) and `models`
(allowlist and escalation ladder) drive routing, `integrate` enables the merge queue,
`require_sandbox` (default true) restricts routing to sandbox-ready workers,
`max_questions` (default 3) is the ask budget.
`generation` starts at 1 and increments on every edit; edits carry the expected
generation and return 409 on a stale write. `prompt` may use `{{repo}}`. Defaults:
`executor = claude-code`, `concurrency = 1`, `budget_class = normal`, `priority` from
the project baseline, `autonomy` inherited (NULL = inherit), `verification` NULL =
the mode's level (a routine may only raise the level, e.g. to `L3`, never lower it).
`allowed_tools` may only *narrow* the mode's tool list (the effective list is the
intersection); a routine naming a tool its mode does not allow is rejected on save.

**Autonomy precedence** (one home, `model.ResolveAutonomy`): Work submit override >
routine > repository `forge.toml [defaults]` > project. Whatever `MODES.md` calls the
"resolved default" is the result of this chain.

`routine_generations(routine_id, generation, snapshot JSON, prompt_version_hash,
created_at, source)` keeps every generation so A/B comparisons (§12) and "what ran"
questions have an exact answer. `source` is `edit` or `proposal:<id>`.

### Workflow

Routines strung together. `workflows(id, name UNIQUE, steps JSON, schedule,
schedule_enabled, generation, next_due_at, archived_at)` with the same
generation/409, archive, and snapshot (`workflow_generations`) machinery as
routines. `steps` is an ordered list of `{name, routine, after: [{step, on,
stack_on}]}`; `after` may only reference an *earlier* step (a DAG by
construction), a step without `after` follows the previous one (a plain list is
a chain; `after = []` makes an independent root), and steps reference routines
by name — latest generation at instantiation time, the Work snapshot being the
audit trail.

A workflow is definition-layer only: `POST /api/v1/workflows/{name}/run`
instantiates one Work per step in one transaction, with `after` becoming
ordinary `work_dependencies` edges (§10.3) and each Work stamped with
`workflow_run_id`/`workflow_name`/`workflow_step`. From there the queue,
dependency, failure (`dependency_failed` → attention), and stacking machinery
apply unchanged. A run has no state row — `GET .../runs` groups Works by run id
and derives an aggregate. Data passing between steps beyond `stack_on` branches
is deliberately out of scope.

### Work

`work(id, routine_id NULL-able, routine_name, generation, title, trigger, snapshot
JSON, priority, budget_class, autonomy, integrate, paths JSON, deps JSON, tier,
models JSON, plan_batch_id, workflow_run_id, workflow_name, workflow_step,
prompt_hash, scheduled_for, submitted_by, external_refs
JSON, created_at, finished_at)`. The three `workflow_*` columns stamp Works a
workflow run instantiated (NULL otherwise). `integrate`/`paths`/`deps`/`tier`/`models` are
frozen from the routine (or `task add --integrate/--paths/…`; ad-hoc default
`integrate = false`) so `model.IsTerminal(state, work.integrate)` has a stable
input; `plan_batch_id` comes from `plan` mode (§20); `prompt_hash` is what intake
dedupes on (§22). Wherever this document says a Target is "terminal" it means
`model.IsTerminal(state, work.integrate)`, and "success" means
`model.IsSuccess(state)` (`succeeded`, the merge states, `merged`). `trigger ∈ {manual, schedule, proposal,
dependency, plugin}`. **The user-facing word is "task"**: `forge task add` creates a
Work; the UI, CLI, and docs say task; Work and Target remain the internal names
and never appear in the UI. `task add` without `--routine` creates an **ad-hoc**
Work: `routine_id NULL`, `routine_name = "ad-hoc"` (a valid name; the UI shows "ad
hoc"), an inline snapshot built from
the flags (`--mode`, `--model`, `--autonomy`, …) and the mode's defaults. `snapshot` is the
frozen routine (byte-for-byte what ran). `priority` is mutable (queue reordering);
`finished_at` is written once, at the terminal transition (it bounds the 24 h
attention window); everything else is immutable after creation. Blocked and deferred
reasons are derived at read time (§4.2), never stored. `routine_id` is NULL for Work
Forge creates without a routine (the `verify` Work of §L2); such Work is outside any
routine's `concurrency`.

`work_dependencies(work_id, blocked_by_work_id, on, stack_on)` with `on ∈ {success,
terminal}`; `stack_on` is a property of the edge (§20). For an integrating
dependency `on: success` is satisfied at `merged`, or — with `stack_on` — as soon
as its agent work succeeded (the dependant starts on its branch head).
Inserting an edge that would create a cycle is rejected (`model.WouldCycle` over the
dependency graph of non-terminal Work).

Work state is **derived**, never stored (§4.2).

### Provenance

Every Work carries where it came from and which root intent it serves, so the
tree is walkable both ways: backward ("why does this exist?") and forward ("what
did it spawn?"). The root of a tree is itself a Work — a manual submission, a
routine firing, a plan — never a new entity.

- **`caused_by_work_id`** is the Work whose *execution* created this one — the
  immediate cause. NULL for a root.
- **`root_work_id`** is the root of this Work's tree: its own id for a root, else
  the parent's root. It is denormalized on purpose so "give me the whole tree" is
  one indexed query, not a recursive walk; `CreateWork` is the one place that
  computes it and it is NOT NULL for every row it writes.
- **`cause`** is a short machine label for the functional reason —
  `plan_task`, `verify`, or `follow_up` — empty for roots
  (`model.Cause`, validated alongside `Trigger`).

The invariant is enforced in the store, not by callers: setting
`caused_by_work_id` looks up the parent (which must exist) and copies its root; a
root's `root_work_id` is its own id. The three spawn sites stamp it —
`planFollowUps` (`plan_task`), `createFollowUp` (`verify`/`follow_up`), and an
explicit `caused_by` on a work request — while a manual, routine-run, or
workflow-step Work is a parentless root (a workflow run groups its siblings by
`workflow_run_id`, not by a fabricated parent).

This is distinct from **dependency edges** (`work_dependencies`, §10.3): a
dependency is a *scheduling* relation ("B may not start until A succeeds"),
whereas provenance is a *causal* one ("A's run created B"). A plan batch has
both — the plan is every task's cause, and `blocked_by` edges order the tasks —
but the two answer different questions and neither is derivable from the other.
`plan_batch_id` and the snapshot's `verify_of` are kept as-is (existing readers);
the provenance columns are the walkable superset, redundant on purpose. The
lineage API (`GET /api/v1/work/{id}/lineage`) and the `/work/{id}` view read this
tree; the schema lives in the `work_provenance` migration.

### Target

`targets(id, work_id, repository_name, state, worker_id, lease_token_hash,
lease_expires_at, cancel_requested, retained, failure_reason, unverified_reason,
external_refs JSON, claimed_at, started_at, finished_at)`. One per repository in the Work; unique on
`(work_id, repository_name)`. `worker_id` is set at first claim and pins later claims
(resume) to the worker that owns the worktree; a pinned Target whose worker is not
connected stays `pending` with a visible reason (`waiting for worker <name>`) — it is
never failed automatically, a human may cancel it.

### Attempt

`attempts(id, target_id, worker_id, claim_request_id UNIQUE, mcp_token_hash,
executor, runner, model, model_alias, escalated_from, routing JSON, sandboxed,
effort, mode, autonomy, worktree_path, branch, base_branch, base_commit,
stack_base_commit, head_commit,
pid, pid_start, session_id, prompt_version_hash, launches, started_at, finished_at,
exit_code, failure_reason, unverified_reason, is_error, result_text, result JSON, num_turns, input_tokens,
output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, git_dirty,
git_commits, git_files_changed, git_insertions, git_deletions, git_pushed,
verification_level, verification_passed, cleanup_outcome, cleanup_reason,
cleanup_command, output_path, output_bytes, output_truncated)`.

`model` is the resolved `[models.<alias>].id` handed to `{{model}}` and confirmed by
`system/init`; `model_alias` is what the routine or router chose. `base_commit` is
the commit the worktree was created at — the integration branch head, or the
dependency's branch head when stacked — and is what `git_inspect`, cleanup, and L0
measure against; `stack_base_commit` is the integration-branch head at claim, the
later rebase target (§20). `mcp_token_hash` is the per-attempt token `forge mcp`
presents (§13). One Attempt per Target for now; the table allows many. A Target that pauses in
`waiting_human` and resumes keeps the **same Attempt** (same worktree, branch, and
`session_id`); `launches` counts agent processes; the manifest carries `elapsed_before_us` and
`next_seq` from the previous launch so `elapsed_us` and `seq` continue monotonically
across launches (and exclude human waiting time) instead of colliding with the
earlier launch's rows.

### Question

`questions(id, attempt_id, target_id, work_id, text, options JSON, context JSON,
checkpoint, answer, answered_by, asked_at, answered_at)`. Open questions are the rows
with `answered_at IS NULL`.

### Event

`events(attempt_id, source, seq, time, elapsed_us, kind, message, span_id, parent_id,
name, duration_us, attrs JSON)`, primary key `(attempt_id, source, seq)`. `source ∈
{worker, mcp, control}` — each source numbers its own `seq`, so worker batches,
`forge mcp` tool spans, and control-plane spans never collide. `kind ∈ {lifecycle,
stdout, stderr, span_start, span_end, metric}`. Ingested in batches (§8).

### AttemptFacts, RateLimitSample, PromptVersion

Defined in §9. Immutable once written.

### Proposal

`proposals(id, source, kind, target, before JSON, after JSON, rationale,
verification_plan, status, decided_by, decided_at, applied_ref, outcome_metrics JSON,
external_refs JSON, created_at)`. §12.

`external_refs` (Work, Target, Proposal) is `[{plugin, kind, id, url, label}]`,
written by plugins with `annotate:write` and rendered by the UI generically as
links (§17).

### KbNote, KbLink

`kb_notes(id, path, title, type, created, tags JSON, mtime, hash, body_hash)`,
`kb_links(from_id, to_kind, to_ref, link_type)`, FTS5 `kb_fts(id, title, body)`. §11.

### Worker

`workers(id, name, version, max_concurrent, active, executors JSON, capabilities
JSON, registered_at, last_seen_at)` and `retained_worktrees(attempt_id, worker_id,
path, reason, cleanup_command)` reported on every registration. `capabilities` is
discovered at worker start and uses one vocabulary from M1: `browser`
(`ready|missing`; Playwright + Chromium under `<home>/deps`), `sandbox`
(`ready|missing`; `bwrap` present), `executor:<name>` (`ready|missing|
unauthenticated`), `runner:<name>` (`ready|down|unauthenticated`, §21), `steer`
(`ready|missing`, §22). Routing requires what the Target needs (§10.2); the Workers
page (renamed **System** in M7) shows them.

### Plugin

`plugins(name PRIMARY KEY, version, kind ∈ {first_party, third_party}, path,
enabled, scopes JSON, token_hash, cursor, installed_at, enabled_at)`. `cursor` is
the last journal id the plugin acknowledged (§17). Tokens are minted at enable
time with exactly the declared scopes; the plain token is handed to the process
in its environment and only its hash is stored.

### PathLease, Merge, Runner

`path_leases(target_id PRIMARY KEY, repository_name, globs JSON, acquired_at)` — the
only home of live write-set leases: inserted in the claim transaction, deleted when
the Target is terminal (§20); `merges(id, target_id,
repository_name, integration_branch, before_sha, after_sha, rebase_attempts,
outcome, pushed_at, created_at)` — one row per merge-queue pass, and every push is
also a `journal` row (`merge.pushed`) with both SHAs; `runners(name, kind, billing,
capacity, endpoint, health, last_probe_at)` mirrors `[runners]` from `config.toml`
with the worker-probed health (§21). `evals(id, mode, prompt_version_hash,
model_alias, fixture, verified, usd, turns, created_at)` and `backups(id, path,
bytes, created_at)` serve M12.

### Journal

`journal(id INTEGER PRIMARY KEY AUTOINCREMENT, ts, kind, entity_type, entity_id,
payload JSON)`. One row per state change of a Work, Target, Attempt, Question, or
Proposal (`entity_type`), plus `daemon` (`daemon.started|draining|restarted|
leases_extended`) and `plugin` (`plugin.enabled|disabled|forbidden|restarted`) rows,
written **in the same transaction** as the change by the
single store helper every state-changing method calls (`store.journal(tx, …)`). `id`
is the monotonic order of events across the whole system; `kind` names the change
(`target.transition`, `work.priority`, `question.answered`, `proposal.decided`, …);
`payload` holds `{from, to, reason, actor}` plus whatever the change needs to be
replayed by a reader. It is the audit trail — the answer to "what happened to X and
when" — and is never reconstructed from logs (§15). Kept forever; `forge prune`
never touches it.

### Verification, Artifact

`verifications(id, attempt_id, level, passed, verifier_attempt_id, verdict JSON,
decided_by, created_at)` — one row per level attempted; `attempt_id` is always the
**subject**, `verifier_attempt_id` the `verify` attempt for L2 (NULL otherwise).
`attempts.verification_level/_passed` are the summary of these rows and the only
other home; facts copy them. `artifacts(id, attempt_id,
kind, path, bytes, sha256, created_at)` — screenshots and other files a `verify`
attempt stores.

## 4. State machines

### 4.1 Target

One transition function, `model.Transition(from, to State) error`, is the only code
that changes a Target's state. The table is the whole rule:

```
pending          → claimed | cancelled
claimed          → preparing | failed | cancelled
preparing        → running | failed | cancelled
running          → waiting_human | verifying | failed | cancelled
waiting_human    → pending | cancelled
verifying        → succeeded | unverified | cancelled
succeeded        → queued_for_merge            (only when the Work has integrate = true)
queued_for_merge → merging | cancelled
merging          → merged | conflict | unverified | queued_for_merge | cancelled
conflict         → queued_for_merge | cancelled (a human resolved it, or gave up)
unverified, failed, cancelled, merged → (terminal; no edges)
succeeded        → (terminal when integrate = false)
```

`merging` is owned by a **worker**: the integrator runs under a *merge claim*
(`POST /api/v1/worker/claim` hands out merge work the same way it hands out
attempts, serial per repository), holds a lease, and heartbeats; `merging →
merged` on push, `→ conflict` when the rebase fails and `integrate` mode cannot fix
it, `→ unverified` (`check_failed:<name>`) when the declared checks fail on the
rebased result, `→ queued_for_merge` when its lease expires (the sweeper's rule:
`rebase_attempts++`, and after `[integration] max_rebase_attempts`, default 3,
`→ conflict`), `→ cancelled` by a human. `conflict → queued_for_merge` is `forge
task requeue ID` after the human fixed the retained worktree.

The four merge states (§20, M9) are in the table and the store from M1 so the
transition function never grows a second home; until M9 nothing sets
`integrate = true` and `succeeded` is simply terminal. `model.IsTerminal(state,
integrate)` is the one place that knows `succeeded` is terminal only without
integration.

Notes:

- The spec's `waiting_human ↔ running` is realised as `waiting_human → pending →
  claimed → preparing → running`: answering re-queues the Target (pinned to the
  worker that owns the worktree); a resume must reacquire a slot, and the same phases
  run (with `fetch`/`resolve_base`/`worktree_add` recorded as skipped). `waiting_human`
  and `pending` hold no slot and no lease.
- On `complete`: if an open Question exists for the attempt (from a `needs_input`
  result or a `forge_ask` call) and autonomy allows it, `running → waiting_human`;
  at `auto` that is `failed:ambiguity_at_auto`. Otherwise, if the process exited
  without error and a structured result parsed, `running → verifying`; the worker
  has already decided L0 and L1, so in the same transaction the Target moves on to
  `succeeded`/`unverified` unless L2 or L3 is required, in which case it stays
  `verifying` **without a lease** until the `verify` Work is terminal or a human
  decides. Every Target passes through `verifying` — there is exactly one path.
- `retained` is a flag, never a state. A Target in `succeeded` (integrating),
  `queued_for_merge`, `merging`, or `conflict` keeps its worktree and branch by the
  `awaiting merge` cleanup row (§6); its manifest sits in the side state
  `awaiting_merge` (§4.3), which the worker does touch again — at merge time.
- `failure_reason` (one home, `model.FailureReason`): `exit_nonzero`, `timeout`,
  `cancelled`, `lease_expired` (used by both the sweeper and a worker that lost its
  lease), `worker_restart`, `launch_failed`, `prepare_failed`, `ambiguity_at_auto`,
  `result_unparseable`, `budget_exceeded` (the executor stopped at the routine's
  `max_budget_usd` — a per-routine cap, never the budget policy, which only defers
  admission), `worktree_lost`, `internal`. `unverified_reason` (`model.UnverifiedReason`):
  `l0:<check>`, `check_failed:<name>`, `check_claim_mismatch:<name>`,
  `verify_verdict:<fail|inconclusive>`, `verify_attempt_failed`, `human_rejected`.
- Cancellation of a `pending`/`waiting_human`/`verifying` Target is immediate; of a
  leased one it sets `cancel_requested`, which the worker sees on its next heartbeat.
  A cancelled resumable attempt is found by the worker's next reconcile (§7.3 step
  3), which treats a terminal Target as no longer resumable and cleans or retains.

### 4.2 Work (derived)

`model.DeriveWorkState(targets, deps, deferred)`:

1. If every Target is terminal: `merged` if all merged; `succeeded` if all
   succeeded; `unverified` if all are succeeded/unverified with ≥ 1 unverified;
   `cancelled` if all cancelled; `failed` if all failed; otherwise `partial`.
2. Else if any Target is `waiting_human` → `waiting_human`; else if any is
   `conflict` → `conflict` (a human must act; it is in the attention list).
3. Else if any Target is `claimed`/`preparing`/`running`/`verifying` → `running`.
4. Else if the Work integrates and any Target is `succeeded`/`queued_for_merge`/
   `merging` → `merging`.
5. Else if a dependency is unsatisfied (§10.3) → `blocked`.
6. Else if the budget policy currently defers this Work's class → `deferred`.
7. Else `pending`.

Attention items are defined once, in §10.4.

### 4.3 Manifest lifecycle (worker-local)

`preparing → worktree_created → running → exited → cleaned | retained`, with side
states `not_created` (finished before the worktree existed), `inconsistent`
(filesystem and Git disagree; never repaired automatically), `missing`, and
`awaiting_merge` (an integrating attempt's worktree, kept until the integrator has
merged it or a human resolved a conflict; after a merge the worker fast-forwards the
local task branch to the rebased head so the normal removal rule — clean, and head
reachable from a remote ref — applies and the worktree is removed, the branch kept).
A manifest is deleted only in `cleaned`, `not_created`, or `missing`, and only after
the control plane has acknowledged the attempt's final cleanup fields.

## 5. Attempt sequence

Each numbered step is a phase span (§8) unless noted. The worker records
`start := time.Now()` at claim; every event's `elapsed_us` is derived from it.

0. **Claim** (control plane). `POST /api/v1/worker/claim` in one transaction: pick the
   first admissible Target (§10.2), create the Attempt, set `claimed`, store the
   lease token's SHA-256 with a 30 s expiry. The control plane computes the
   `queue_wait` span from `work.created_at` to `claimed_at` (its own clock, wall
   time — the one duration that necessarily spans processes; it is stored as a span
   for uniformity but flagged `attrs.clock = "wall"`).
1. **Validate** (lifecycle event). IDs well-formed; repository advertised by this
   worker; executor registered; timeout in range. Failure → `failed:prepare_failed`
   without a worktree.
2. **`fetch`.** Under the per-repository lock: verify origin identity still matches the
   pin; `git fetch --no-tags origin <base>` in the checkout (updates
   `refs/remotes/origin/<base>` and `FETCH_HEAD` only; never the working tree, index,
   or HEAD). Fetch is **best-effort**: on failure, if `refs/remotes/origin/<base>`
   exists locally the attempt proceeds with `attrs.fetch = "failed"` and the error in
   the span; if it does not, the attempt fails `prepare_failed`. Verify origin
   identity again after (Factory's TOCTOU guard).
3. **`resolve_base`.** Base branch = repository `base_branch` → `forge.toml`
   `[defaults].base_branch` → `git symbolic-ref refs/remotes/origin/HEAD` → error
   "set base_branch". Base commit = `git rev-parse --verify
   refs/remotes/origin/<base>^{commit}`; must be a full SHA.
4. **`worktree_add`.** Pre-checks: `data_dir/worktrees` is a real directory (not a
   symlink), the attempt path does not exist. Write the **intent manifest**
   (`preparing`, 0600, atomic) *before* the mutation. Then `git worktree add -b
   forge/<slug>-<attempt8> <path> <base_commit>` in the checkout. On failure, inspect:
   if both the path and the Git registration exist, treat it as created; if exactly
   one does, mark the manifest `inconsistent` and fail; if neither, fail cleanly.
   Release the repository lock. Read `forge.toml` from the worktree.
5. **`manifest`.** Rewrite the manifest as `worktree_created` with branch, base,
   paths, the launch deadline, and `elapsed_before_us` / `next_seq` carried over from
   a previous launch (0 for the first). Write the MCP config (`{"mcpServers":
   {"forge": {"command": "<os.Executable()>", "args": ["mcp", "--attempt",
   "<id>"]}}}` — an absolute path; the agent's shell may not have `forge` on PATH). Render the prompt (`MODES.md`
   §"Prompt assembly"), compute the `PromptVersion` hash, and send it with the
   `preparing` heartbeat so it exists before the agent starts.
6. **`agent`.** Launch the executor: cwd = worktree, prompt on stdin, `Setpgid`,
   deadline = `timeout − elapsed_before_us` measured from this launch, stdout consumed line
   by line by the named parser, stderr tailed (last 64 KiB) and mirrored, both mirrored
   to the raw output file (bounded at 64 MiB, truncated flag). Record `pid` and
   `pid_start` (`/proc/<pid>/stat` field 22) in the manifest (`running`) and in the
   `running` heartbeat **before** anything else happens. Tool spans, usage metrics,
   and `rate_limit` metrics come from the parser, nested under `agent-<launch>`.
   Events are batched (§8). Cancel/timeout/lease-loss/shutdown → SIGTERM the group,
   wait 5 s, SIGKILL, with the pid identity re-verified before each signal.

   The **heartbeat goroutine runs from claim until `complete` returns** — not only
   during this phase: L1 checks in step 8 can take minutes against a 30 s lease. Each
   heartbeat (every 10 s) renews the lease, carries the current phase, and returns
   `cancel_requested`.
7. **`git_inspect`.** In the worktree: `git --no-optional-locks status --porcelain=v1`
   (dirty = any line, untracked included); `git rev-parse HEAD`; `git rev-list --count
   <base>..HEAD`; `git diff --numstat <base>..HEAD` (files, insertions, deletions);
   pushed = `git for-each-ref --contains HEAD refs/remotes` non-empty (trivially true
   when HEAD == base).
8. **`verify`.** Parse the structured result (or record `result_unparseable`). Apply
   L0; if the mode requires L1, run the declared checks (`VERIFICATION.md`). Report
   the achieved level and outcome. Whether the Target ends `succeeded`, `unverified`,
   or waits for L2/L3 is decided by the control plane on `complete` (§4.1).
9. **`cleanup`.** `worker.DecideCleanup` (§6), then act. Manifest → `cleaned` or
   `retained` with the reason and the exact `forge cleanup <attempt> --confirm`
   command. Before reporting, the worker writes `next_seq`, `elapsed_before_us`, and
   `resumable` into the manifest so a resume can continue the timeline.
10. **Complete.** `POST /api/v1/attempts/{id}/complete` with the lease token, exit
    status, result, usage, git outcome, verification result, and cleanup fields. If the
    lease has already expired the completion is still sent — the control plane
    accepts a late completion for a Target it closed as `lease_expired` only for the
    cleanup and git fields, and records `attrs.late = true` (nothing is silently
    lost; smoke 8). Then the control plane computes AttemptFacts (§9.2).

A `needs_input` result at step 8 (autonomy < `auto`) becomes: write the Question,
Target → `waiting_human`, manifest `exited` with `resumable = true`, worktree and
session kept, slot freed. At `auto` it is `failed:ambiguity_at_auto`. Answering
(`forge task answer`, UI) re-queues the Target as `pending` pinned to the owning worker;
the resume launch runs steps 5–10 with `--resume <session_id>` and the answer as the
prompt, continuing `seq` and `elapsed_us` from the manifest.

Failure inside a phase ends that phase's span with `attrs.error = "<short reason>"`
before the lifecycle event that reports it; later phases still run where they make
sense (`git_inspect` and `cleanup` always run once a worktree exists).

## 6. Cleanup rules

One home: `worker.DecideCleanup(in CleanupInput) CleanupDecision`, a pure function
tested by table. Inputs: path exists, Git registration exists, dirty, head, base,
pushed, mode write scope, and whether the attempt is resumable.

| Condition (first match wins) | Decision | Recorded reason |
|---|---|---|
| resumable (`waiting_human`) | keep | `awaiting human answer` |
| awaiting merge (Target in `succeeded` with integrate, `queued_for_merge`, `merging`, `conflict`) | keep | `awaiting merge` |
| greenfield project directory (design pending, `MODES.md` §greenfield) | keep | `greenfield project` |
| path missing and not registered | nothing to remove | `worktree missing` |
| exactly one of path / registration exists | retain, mark `inconsistent` | `worktree exists in only one of filesystem and git registry` |
| dirty (`status --porcelain` non-empty) | retain | `dirty worktree` |
| head ≠ base and not pushed | retain | `unpushed commits` |
| clean and (head == base or pushed) | `git worktree remove <path>` (never `--force`) | `removed` |
| removal failed | retain | the git error |

After removal the worker re-checks that the path is gone and `git worktree list
--porcelain` no longer lists it; a "success" that leaves either behind is an error and
the attempt is retained. **The branch is never deleted by Forge.** The registered
checkout is never modified. Retained attempts record `forge cleanup <attempt>
--confirm`, which removes the worktree with `--force` (uncommitted changes are lost,
after a preview) and still keeps the branch. Cleanup does not depend on whether the
attempt succeeded: a clean, unpushed-free worktree of a failed attempt is removed too —
the raw output file and events are the record.

## 7. Worker

### 7.1 Start

Read `~/.forge/worker.toml`:

```toml
daemon        = "unix://~/.forge/forge.sock"   # or http://127.0.0.1:7340 with token_file
token_file    = "~/.forge/token"
name          = "laptop"
max_concurrent = 4
data_dir      = "~/.forge/worker"

[executors.claude-code]                # §7.4
command = [...]
output  = "claude-stream-json"
capabilities = [...]
sandbox = true                         # wrap with bubblewrap when available (§19)

[repositories.equitizr]
path = "~/Projects/equitizr"
base_branch = "master"
project = "default"

[repositories.forge]            # written by `forge init`; retro and code proposals target it
path = "~/Projects/forge"

[greenfield]                    # pending the greenfield decision (MODES.md)
projects_root = "~/Projects"
```

Then: `flock` the data directory (a second worker on the same data dir fails with a
clear error); validate every repository — canonical path, `git rev-parse
--is-inside-work-tree` prints `true`, `git remote get-url origin` non-empty,
`base_branch` (if set) passes `git check-ref-format`, no two names resolve to the
same path or identity; pin the normalised origin identity (`host/path`, `.git`
stripped, GitHub compared case-insensitively); check the executor commands exist;
**reconcile** (§7.3); register; then loop.

### 7.2 Loop

- Registration every 30 s (doubles as liveness; a worker is connected if seen in the
  last 90 s), carrying repositories, executors, active count, and retained worktrees.
- Claim polling: one claim request in flight at a time, every 2 s (+ up to 20 %
  jitter) while a slot is free; a successful claim immediately tries again so a
  queue fills all slots without waiting a poll interval per slot.
- Slots: a buffered channel of `max_concurrent` tokens; preparing attempts hold a slot.
- Per-repository mutex around fetch / resolve / worktree add / worktree remove so two
  attempts on one checkout serialise their Git metadata operations; the lock is
  released before the agent starts.
- Reconcile every 5 minutes. Manifests in `retained` are final and never touched
  again by the worker, so `forge cleanup` can act on them while the worker runs.
- Shutdown (SIGINT/SIGTERM): stop claiming, send cancel to every active attempt,
  wait up to 30 s, report what can be reported; whatever remains is handled by
  reconcile on the next start.

### 7.3 Reconcile

Manifests are the worker's source of truth for "what did I start". On start and every
5 minutes, for every manifest not in a final state and not owned by an attempt running
in this process:

1. If the manifest records a pid and `/proc/<pid>/stat` shows the same start time,
   the process is an orphan of a crashed worker: SIGTERM its group, wait 5 s, SIGKILL.
   If the pid is alive with a *different* start time it is someone else's process —
   never signalled; the manifest is marked `inconsistent` and reported.
2. Inspect Git exactly as in §5.7 and apply §6.
3. `GET /api/v1/attempts/{id}` to learn what the control plane believes. A manifest
   marked `resumable` whose Target is still `waiting_human`/`pending` is left alone
   (that is the normal paused state); if its Target is terminal (cancelled, or failed
   by a human) it is no longer resumable and falls through to cleanup. If the Target
   is still leased and non-terminal, `complete` it as `failed:worker_restart` with the
   git and cleanup fields; if the control plane already closed it (`lease_expired`),
   `PATCH /api/v1/attempts/{id}/cleanup` with the cleanup outcome. Either way the
   attempt does not vanish: it is `failed` and either cleaned or retained.
4. Manifest → `cleaned` / `retained` / `not_created` / `missing`; the reconcile
   report (count per outcome, each retained path and reason) is logged and sent with
   the next registration.

Manifests are read with the same strictness they are written: regular file, mode
`0600`, no unknown fields, no trailing JSON, worktree path equal to the owned path,
branch equal to the derived name. Anything else is `inconsistent` — refused, never
"repaired" by deleting.

### 7.4 Executor contract

```toml
[executors.claude-code]
command = ["claude", "--print", "--verbose", "--output-format", "stream-json",
           "--dangerously-skip-permissions", "--strict-mcp-config",
           "--model", "{{model}}", "--max-turns", "{{max_turns}}",
           "--mcp-config", "{{mcp_config}}"]
output = "claude-stream-json"
capabilities = ["allowed_tools", "builtin_tools", "json_schema", "resume",
                "max_budget_usd", "effort", "append_system_prompt"]
```

`worker.Executor` is an interface: `Command(ctx, LaunchSpec) *exec.Cmd`,
`Capabilities()`. One implementation ships: the template executor above. The test
executor **`fake-claude`** is not a second implementation but a hidden subcommand,
`forge fake-claude --fixture DIR`, reached through the same template executor
(`[executors.fake-claude] command = ["<forge>", "fake-claude", "--fixture",
"{{fixture}}", …]`, `output = "claude-stream-json"`), so tests differ from
production only in `worker.toml` and go through the same launch, parser, and
sandbox path. A fixture directory holds `script.jsonl` (the stream-json lines to
emit) and `meta.toml`:

```toml
exit_code = 0
delay_ms = 5                       # between lines; per-line overrides below
resume_script = "resume.jsonl"     # emitted instead when --resume <session> is given
needs_input_at = 12                # stop after this line with a needs_input result
rate_limit_event = { five_hour = 0.41, seven_day = 0.22 }   # emitted before result
[[files]]                          # written into the cwd when the given line is emitted
at_line = 4
path = "FORGE_SMOKE.txt"
content = "…"
git_commit = "chore: forge smoke test"   # optional: commit after writing
[[line_delays]]
line = 7
delay_ms = 3000
```

Six fixtures are required (`testdata/fixtures/`): `inventory` (read-only),
`commit` (writes and commits), `failing` (non-zero exit, `is_error`),
`needs-input` (pause and `resume.jsonl`), `timeout` (a long delay the worker must
kill), `tool-error` (a `tool_result` with `is_error`). M1 ships them hand-authored
from §7.5's event shapes; the M1 smoke records real haiku runs, which — scrubbed of
paths, ids, and timestamps — replace the hand-authored ones. Every Go and browser
test runs `fake-claude`; real Claude runs only in `just smoke`; `just check` passes
with no network and no `claude` on `PATH` (`just check-offline`). The launch path
has one wrapper hook — `Sandbox.Wrap(cmd)` (§19) — so M8's bubblewrap wrapping does
not touch the executor. `{{model}}` receives the resolved model id, never the alias.
Git options for Forge-owned worktrees (`merge.conflictstyle=zdiff3`, the `mergiraf`
merge driver, `rerere`) are passed with `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/
`GIT_CONFIG_VALUE_n` on the processes Forge launches — never by writing any
`.git/config` or the user's global config. Each capability the executor declares
*and* the routine/mode uses appends flags:

| capability | flags |
|---|---|
| `allowed_tools` | `--allowedTools <comma list>` (Forge tools prefixed `mcp__forge__`) |
| `builtin_tools` | `--tools ""` when the mode disables built-ins |
| `json_schema` | `--json-schema <mode result schema>` |
| `resume` | `--resume <session_id>` (the answer becomes the prompt) |
| `max_budget_usd` | `--max-budget-usd <n>` |
| `effort` | `--effort <level>` |
| `append_system_prompt` | `--append-system-prompt <text>` |
| `steer` | `--input-format stream-json` so `forge task tell` can inject a user turn (§22) |

Template variables: `{{model}} {{max_turns}} {{repo}} {{worktree}} {{mcp_config}}
{{session_id}} {{fixture}}`. Everything the worker needs from the daemon's config
for one attempt — `require_sandbox`, `allow_hosts`, the `GIT_CONFIG_*` values —
arrives in the claim's `policy` block; the worker has no second config file. cwd = worktree; prompt on stdin; exit 0 is success; the worker owns
the timeout. Every flag above is verified against `claude --help` at worker start and
a missing one fails registration with the flag named (the CLI changes often).

### 7.5 Parsers

`worker.OutputParser` interface: `Line(line []byte) []protocol.Event` (the worker
assigns `seq`, `time`, `elapsed_us`) and `Result() ParseResult`. Registered by name.

`claude-stream-json` recognises:

- `system/init` → `session_id`, `model`, `tools` (metric `init`, lifecycle event).
- `assistant` messages → for each `tool_use` block, `span_start` (name = tool name,
  `span_id = hex(sha256(block.id))[:8]`, attrs `{tool, input_bytes, input_summary}`;
  MCP tool calls carry `attrs.mcp = true`); `usage` → a `usage` metric keyed by
  `message.id` (the same message is emitted once per content block with identical
  usage — **last write per message id wins, never summed**).
- `user` messages → each `tool_result` block closes the span whose id matches
  (`span_end`, attrs `{output_bytes, is_error}`).
- `rate_limit_event` → metric `rate_limit` with both windows' `utilization` and
  `resets_at`.
- `result` → `is_error`, `num_turns`, `total_cost_usd`, `usage` (**authoritative
  totals for the attempt**, including cache fields), `duration_ms`, `result` text,
  `structured_output` when `--json-schema` was used (verified against the live CLI
  in M1; if the field name differs the parser adapts, the contract does not).
- Everything else is ignored but counted (`metric unknown_lines`).

Lines are framed on `\n` with a 1 MiB cap; an over-long line is dropped with a
`stderr`-kind event naming its size and the parser continues. Non-JSON lines become
`stdout` events (subject to the cap). `lines` is the generic fallback: every line is a
`stdout` event; the result text is the last non-empty line.

## 8. Events and spans

```go
type Event struct {
    Seq        int             `json:"seq"`
    Time       time.Time       `json:"time"`                  // UTC
    ElapsedUS  int64           `json:"elapsed_us"`            // monotonic since attempt start
    Kind       string          `json:"kind"`                  // lifecycle|stdout|stderr|span_start|span_end|metric
    Message    string          `json:"message"`
    SpanID     string          `json:"span_id,omitempty"`
    ParentID   string          `json:"parent_id,omitempty"`
    Name       string          `json:"name,omitempty"`
    DurationUS int64           `json:"duration_us,omitempty"` // span_end only
    Attrs      json.RawMessage `json:"attrs,omitempty"`       // small; never raw tool output
}
```

Phase spans, in order, all children of the attempt root (`span_id` = attempt short
ID): `queue_wait` (source `control`), `fetch`, `resolve_base`, `worktree_add`,
`manifest`, `agent-<launch>`, `git_inspect`, `verify`, `cleanup` (source `worker`).
Tool spans from the parser are children of the current `agent-<launch>`. Forge tool
calls are spans emitted by `forge mcp` (source `mcp`, `span_id = mcp-<n>`, parent
`agent-<launches>` read from the attempt row) with `attrs {tool, input_sha256,
output_sha256, input_bytes, output_bytes}`; their `duration_us` is monotonic inside
`forge mcp`.

Clocks: `elapsed_us` for worker events is monotonic from the attempt start (carried
across launches). Events from other sources cannot share that clock; their
`elapsed_us` is `time − attempts.started_at` and they carry `attrs.clock = "wall"`.
`queue_wait` has `elapsed_us = 0` and a wall-clock duration. Durations are always
measured monotonically by the process that measured them; positions on the timeline
are the only wall-derived numbers, and they are flagged.

Caps: `stdout`/`stderr` events are capped at 2000 events / 1 MiB per attempt; after
the cap a single lifecycle event records `events_dropped` and the count. Spans,
metrics, and lifecycle events are never dropped.

Batching: the worker buffers events and flushes every 500 ms, or at 100 events, or at
256 KiB, whichever first; `POST /api/v1/attempts/{id}/events` inserts the batch in
one transaction with a prepared statement. Delivery is at-least-once with `seq` as the
idempotency key (`INSERT OR IGNORE`). 4xx disables further sends for the attempt
(logged); 5xx/transport retries with backoff 100 ms → 2 s. The final flush happens
before `complete`.

## 9. Measurement

### 9.1 RateLimitSamples and PromptVersions

`rate_limit_samples(ts, window, utilization, resets_at, source_attempt)` — one row per
window per `rate_limit_event`. These drive the budget policy (§10.1).

`prompt_versions(hash PRIMARY KEY, routine, generation, mode, template,
rendered_example, system_append, tool_list JSON, model, effort, created_at)`. The hash
is SHA-256 over `(mode template, routine prompt, system append, sorted tool list,
model, effort)`; `rendered_example` is the first rendering seen. Every attempt links to
one.

### 9.2 AttemptFacts

One immutable row per attempt, computed by `controlplane/facts.Compute` from the
attempt row, its events, samples, questions, and the Work — once, when the Target
becomes terminal (or, for a lease-expired attempt, when the sweeper closes it; a late
completion updates only git/cleanup columns in `attempts`, never facts).

Columns: identity (`attempt_id, target_id, work_id, routine, generation, project,
repository, worker, executor, model, effort, mode, trigger, prompt_version_hash,
autonomy`); time (`queue_wait_us, fetch_us, resolve_base_us, worktree_add_us,
manifest_us, agent_us, git_inspect_us, verify_us, cleanup_us, total_us, started_at,
finished_at`); agent (`turns, input_tokens, output_tokens, cache_read_tokens,
cache_creation_tokens, cost_usd NULL-able, tool_calls_total, tool_calls_by_name JSON,
tool_time_us_by_name JSON, tool_p50_us, tool_max_us, tool_errors, questions_asked,
wait_human_us, events_total, events_dropped`); outcome (`state, exit_code,
failure_reason, is_error, verification_level, verification_passed, retained,
retained_reason`); git (`commits, files_changed, insertions, deletions, dirty, pushed,
branch, base, head`); budget (`five_hour_before, five_hour_after, seven_day_before,
seven_day_after, utilization_delta_estimate`); integration (`declared_paths JSON,
touched_paths JSON, write_set_precision` = |touched ∩ declared| / |touched|,
`lease_wait_us, merge_wait_us, rebase_attempts, merge_outcome, stack_depth`); cost
vector (`usd` = tokens × price, notional on subscription; `five_hour_delta`,
`seven_day_delta` measured, subscription only; `runner_seconds`); routing (`runner,
model_alias, model_class, escalated_from, tokens_to_first_edit`); human loop
(`questions_changed_outcome`; `questions_asked` is in the agent group). All present from the first migration, NULL until the milestone that computes them.
Facts are computed at the attempt-terminal transition (`succeeded`, `unverified`,
`failed`, `cancelled`); for integrating Work the integrator later fills the five
integration columns (`merge_wait_us`, `rebase_attempts`, `merge_outcome`,
`stack_depth`, and `touched_paths` if a rebase changed them) exactly once, NULL →
value — the single exception to "immutable once written" (STYLE.md §6).

Rules: phase times are the matching span's `duration_us` (sum over launches for
`agent`); `total_us` is the sum of every phase span over every launch (so it excludes
human waiting by construction); `tool_*` come from tool and `mcp` spans;
`wait_human_us` is the sum of the attempt's Question open intervals measured by the
control plane (wall clock, because it spans processes — the only such column);
`*_before` is the last sample before `started_at`, `*_after` the last sample of this
attempt; `utilization_delta_estimate = five_hour_after − five_hour_before` when both
came from this attempt's own samples, else NULL. Anything not computable is NULL,
never 0. Indexes: `(routine, finished_at)`, `(repository, finished_at)`, `(routine,
generation)`, `(prompt_version_hash)`.

### 9.3 Retention

Facts, spans, metrics, samples, prompt versions: forever. Raw transcripts of Claude
attempts: 90 days, gzip-compressed after 7; other raw output: 30 days. Artifacts:
90 days. `forge prune --dry-run | --confirm` and a nightly job apply the policy;
nothing else deletes.

### 9.4 Stats

`GET /api/v1/stats?since=7d[&routine][&repository][&project][&mode]` returns, per
routine and per generation: runs; outcome counts and rates with **verified** success
(`succeeded` with `verification_passed`) separate from self-reported (`is_error =
false`); p50/p95/max of `total_us` and `agent_us`; tokens per run; cost per run and
per verified success; turns; questions per run; tool mix (`tool_calls_by_name`
summed); top failure reasons; utilization consumed (sum of positive
`utilization_delta_estimate`); and `prev_*` for the previous equal window.
Percentiles are computed in Go over rows from an indexed range query.
`forge stats` prints it; `forge retro --since 7d --json` emits the reflection data
pack: the stats with deltas, the current prompt and settings per routine, and for each
failed/unverified/retained/cancelled attempt its facts, structured result, and last
30 non-stdout events.

## 10. Budget and queue

### 10.1 Usage model

For each window `w ∈ {five_hour, seven_day}` with length `L_w`:

- `u_w` — utilization from the latest sample; `resets_at_w` from the same sample;
  `window_start = resets_at − L_w`; `f = (now − window_start) / L_w` (fraction
  elapsed, clamped to [0, 1]); `hours_to_reset = (resets_at − now)`.
- Consumption rate `r_w(h)` over the last `h ∈ {1, 6, 24}` hours: sum of positive
  utilization deltas between consecutive samples in the period, divided by `h`
  (a drop in utilization or a change in `resets_at` is a reset boundary and is not a
  negative delta).
- Calibration: `tokens_per_point` = median over recent facts of `(input + output +
  cache_creation) / utilization_delta_estimate` where the delta is non-NULL and > 0.
- Forecast at reset: `u_w + r_w(1h) · hours_to_reset + Σ_queued(expected_delta)`,
  where a queued Work's expected delta is its routine's median
  `utilization_delta_estimate` (NULL → 0, flagged).
- **Target average**: `target_rate_w = max(0, target_w − u_w) / hours_to_reset` — the
  utilization-per-hour that would land exactly on the target at reset.
- **Delta**: `r_w(1h) − target_rate_w` (positive = burning faster than needed).

`forge usage` and `forge_usage` print all of this per window; the dashboard shows one
gauge per window with the target line.

### 10.2 Admission policy

One home: `controlplane/budget.Decide(now, Usage, class, cfg) Decision{Admit bool,
Reason string}`, pure and table-tested. Evaluated at claim time (claim *is*
admission) and, for display, when listing the queue.

In `<home>/config.toml`:

```toml
[budget]
five_hour_target    = 0.90
seven_day_target    = 0.90
five_hour_hard_stop = 0.97
seven_day_hard_stop = 0.97
daily_usd_cap       = 25.0          # optional; gates all admission (a runner's own cap, §21, gates that runner only)
forecast_pacing     = false         # default; true = also defer when the burn-rate forecast would overshoot target
[budget.quiet_hours]                # optional, local time
start = "09:00"
end = "18:00"
reserve = 0.20
```

In order, per window (both windows must admit):

1. `u ≥ hard_stop` → defer all classes: `hard_stop:<window>`. Running work is never
   killed (constitution 6). Sum of `cost_usd` since local midnight ≥ `daily_usd_cap`
   → `daily_usd_cap`.
2. `interactive` → admit. A dependency-triggered follow-up Work (the L2 verify
   attempt, `trigger = dependency`) is judged as `interactive` here regardless of
   its stored class: its subject already spent the tokens, and a soft deferral
   would strand the subject in `verifying`. Rule 1 still applies to it.
3. Quiet hours: non-interactive classes admit only while `u < target − reserve`
   (`quiet_hours`).
4. `normal` → admit iff `u < target` and, when `forecast_pacing` is on, the 1 h-rate
   forecast at reset ≤ `target` (`forecast_over_target`) — the forecast spreads work
   across the window so a burst does not overshoot by the reset. `forecast_pacing`
   defaults **false**: only current utilization gates, so a transient end-of-session
   spike no longer blocks new work (worst case a burst overshoots and is cut at the
   reset, to be restarted); the hard stop still protects the ceiling. When there are
   no samples yet, admit.
5. `backlog` → admit iff `normal` would admit **and** `u < target · f`
   (`ahead_of_burn_down_line`). This is burn-down: early in a window the line is low
   and backlog waits; as the reset nears the line rises to the target and backlog is
   admitted aggressively to spend what would otherwise be lost.

Also enforced at claim, in this order, each a named reason when it blocks: routine
`concurrency` (Work of that routine with any Target in `claimed`/`preparing`/
`running`; `waiting_human`, `pending`, and `verifying` Targets do not count, and
routine-less Work never counts); a worker slot on a connected worker advertising the
Target's repository, executor, and required capabilities (`sandbox`, `browser`,
`runner:<name>`); a **runner slot** (`[runners.<name>].capacity`, §21); and a
**path lease** on the repository (§20: the Target's globs must not intersect a
running Target's globs; undeclared paths = the whole repository; lockfiles are an
implicit exclusive lease). The claim query evaluates all four in one transaction so
two workers cannot both win. A Work whose class is deferred shows `deferred` with the reason. Measured:
at each reset boundary the scheduler records `target − u_at_reset` as the
`unspent_at_reset` metric (a stats line).

### 10.3 Priority queue

`controlplane/queue.Order(works) []Entry` — one ordered list of every non-terminal
Work: sort by `priority DESC, class rank (interactive > normal > backlog), created_at
ASC`. Each entry is `eligible`, `blocked` (with the unsatisfied dependencies), or
`deferred` (with the budget reason). Defaults: human-submitted Work is `interactive`
with priority 100; scheduled Work uses the routine's priority and class; proposal- and
dependency-triggered Work inherit their source's.

Dependencies: `blocked_by: [{work_id, on}]`. `on: terminal` is satisfied when the
dependency is terminal; `on: success` when it is `succeeded`. If a `success`
dependency ends any other way the dependent Work is `blocked` with reason
`dependency_failed` and stays so until a human removes the edge or cancels it.
`PATCH /api/v1/work/{id}` sets `priority` and adds/removes edges; cycles are refused.
The UI's drag-and-drop refuses an order in which a Work sits above one it is blocked
by; the API enforces the same rule (`queue.Violates`).

### 10.4 Human queue

`GET /api/v1/attention` is the one list of things needing a person, and the
dashboard's "attention" panel is a view of it: open Questions; proposals in
`proposed`; Targets in `verifying` awaiting L3; Targets in `conflict`; Targets
`pending` for more than 5 minutes with no eligible worker (a missing `sandbox`,
`browser`, or `runner` capability, named); `notify`-level checkpoint events from
the last 24 h (informational, no acknowledgement — they age out); Work that ended
`partial`/`failed`/`unverified` in the last 24 h; retained worktrees. Each row: what,
from which attempt/routine, waiting since, and the action. `forge task answer <task>
"…"`, `forge proposal approve|reject <id>`, `forge task approve|reject <task>` (L3).

## 11. Knowledge base

Markdown notes under `[kb] path` in `<home>/config.toml` (default `<home>/kb`). Frontmatter: `id`
(immutable, equals the filename stem), `title`, `type ∈ {retro, hypothesis, proposal,
spec, note}`, `created`, `links: {about: [], supersedes: [], evidence_for: []}`,
`tags`. Inline links `[[id]]`. Fact links use the grammar in §2.

Index: `kb_notes`, `kb_links`, FTS5 `kb_fts`; reindex incrementally by (mtime, hash) on
start, every 5 minutes, and after every write through `forge kb`. `forge kb
new|resolve|backlinks|links|graph|search|check|export`. `check` fails on missing,
duplicate, or mismatched ids, malformed frontmatter, and dangling note or fact links,
and runs in `just check`. `export` rewrites `[[id]]` to relative links into a build
directory; canonical files are never rewritten. Agents create notes only via `forge kb
new` / `forge_kb_new`.

## 12. Reflection

`retro` mode (`MODES.md`) consumes the data pack and produces a kb note and
Proposals. `Proposal.kind ∈ {routine, mode_prompt, doc, tool, process, code}` with the
verification each requires before apply (spec table). Approval is human by default;
`auto_apply_after = {approved: N, reverted: 0}` per kind may be configured and starts
unset.

Applying a `routine` proposal creates a new generation (`source = proposal:<id>`). The
A/B rule: after `K` (default 5) runs on the new generation, compare verified success
rate and cost per verified success against the previous generation's last `K` runs;
if either regresses beyond the configured margin (default 20 % relative), Forge
restores the previous generation (a new generation whose snapshot equals the last
human-approved one), marks the proposal `reverted`, and records `outcome_metrics`.
This is not Forge applying a change of its own: the human's approval covers the A/B
plan *including* its revert, and the revert only ever restores a generation a human
already approved. Approval text says so explicitly. `code` proposals become a `forge/proposal-<id8>` branch on
the Forge repository containing the diff and the proposal text, and stop there.

Funnel stats: proposed → approved → applied → reverted, and cost per applied
(reflection attempts' cost / applied count).

## 13. HTTP API (summary)

Both listeners serve every route; authorisation is by transport and token (§1.1).

Daemon: `GET /api/v1/handshake` → `{version, schema_version, state}`; `POST
/api/v1/daemon/drain`; `GET|POST /api/v1/log-level`; `GET /api/v1/journal?since=<id>`
(SSE, one event per journal row, `id` = journal id, so a client resumes from its
cursor with no gaps); `GET /api/v1/doctor`.

Operator: `GET /api/v1/dashboard`; `GET|POST /api/v1/routines`,
`GET|PUT|DELETE /api/v1/routines/{name}`, `POST /api/v1/routines/{name}/run`;
`GET|POST /api/v1/work`, `GET|PATCH|DELETE /api/v1/work/{id}`; `GET /api/v1/queue`;
`GET /api/v1/attention`; `GET /api/v1/attempts/{id}`, `GET
/api/v1/attempts/{id}/events`; `POST /api/v1/questions/{id}/answer`; `POST
/api/v1/targets/{id}/approve|reject` (L3); `GET|POST
/api/v1/proposals`, `POST /api/v1/proposals/{id}/approve|reject`; `GET /api/v1/stats`;
`GET /api/v1/retro`; `GET /api/v1/usage`; `GET /api/v1/workers`; `GET
/api/v1/repositories`; `GET /api/v1/kb/...`. `/api/v1/tasks[...]` is an alias of
`/api/v1/work[...]` so URLs match the user-facing word; both accept
`external_refs`. `GET /api/v1/work/{id}/stream?since=<journal id>` (SSE: the
Work's journal rows and its attempts' events, each with an `id`) backs `task logs
-f` and `--wait`, which reconnect from their last id until the Work is terminal.

Plugins (scoped token): `GET|POST /api/v1/plugins`, `POST
/api/v1/plugins/{name}/enable|disable`, `POST /api/v1/plugins/{name}/ack
{cursor}`, `GET /api/v1/plugins/{name}/status`; plus whichever operator routes the
token's scopes allow (`work:write` → create Work with `external_refs`;
`annotate:write` → `PATCH …/external_refs`). A request outside scope is 403 and
journaled (`plugin.forbidden`).

Worker (token): `POST /api/v1/worker/register`; `POST /api/v1/worker/claim`; `POST
/api/v1/attempts/{id}/heartbeat`; `POST /api/v1/attempts/{id}/events`; `POST
/api/v1/attempts/{id}/complete`; `PATCH /api/v1/attempts/{id}/cleanup`.

Tools (token, called by `forge mcp`): `POST /api/v1/tools/{name}` with
`{attempt_id, input}` for fact, knowledge, and control tools, which run in the
control plane; the handler dispatches to the registered `Tool`. **Repository tools
(`forge_check`, `forge_repo_status`, `forge_diff_summary`) run inside `forge mcp`
itself** — it is a child of the agent, in the worktree, in the agent's process
group, so a group kill takes its checks with it and the control plane never spawns
processes in a worker-owned directory. Every tool call, wherever it ran, is reported
by `forge mcp` as an `mcp`-source span through `POST /api/v1/attempts/{id}/events`.

## 14. Leases, crashes, and what can never be lost

- Claim → 30 s lease; heartbeat every 10 s renews and returns `cancel_requested`;
  completion requires the lease token (hash compare). A sweeper (every 10 s) moves
  expired-lease Targets in `claimed`/`preparing`/`running` to
  `failed:lease_expired` and computes their facts. **Daemon restarts do not expire
  running attempts:** on start, before the sweeper's first tick, the daemon sets
  `lease_expires_at = now + 120 s` on **every** Target in
  `claimed`/`preparing`/`running` regardless of its stored expiry, and journals
  `daemon.leases_extended`; so a restart of up to 120 s (requirement: ≥ 60 s) is
  invisible to the worker. The worker, for its part, keeps the agent running while
  heartbeats fail, retrying with one shared backoff (1 s → 10 s, also used for claim
  polling while the daemon is down), re-registers immediately on the first success
  (and a heartbeat counts as liveness, so the 90 s "connected" rule never lags a
  live worker), and only declares `lease_expired` locally after 120 s without a
  successful heartbeat. A completion that arrives after that is still accepted for
  its git/cleanup fields (§5.10). `complete` is idempotent: a retry for an attempt
  whose `finished_at` is already set, with the same lease token hash, returns 200
  with the stored outcome. `claim_request_id` is minted per claim attempt and reused only when the
  previous attempt ended in a transport error or 5xx. `pending`, `waiting_human`, and
  `verifying` hold no lease; a `verifying` Target is resolved by its `verify` Work's
  terminal transition or a human, never by the sweeper.
- Worker killed mid-attempt: the lease expires (Target `failed:lease_expired`); the
  restarted worker's reconcile finds the manifest, kills the orphan process group,
  inspects, cleans or retains, and patches the cleanup fields. Nothing leaks (smoke 7).
- Daemon killed mid-attempt: the worker keeps running and retrying heartbeats;
  the next CLI call auto-starts the daemon, which extends live leases before
  sweeping, so the attempt's completion is accepted normally (smoke 8, M6 smoke
  6). Only if the daemon stays down longer than 120 s does the worker stop the
  agent as `lease_expired`; the completion is then accepted for its git/cleanup
  fields.
- Control plane killed mid-claim: the claim transaction either committed (the worker
  gets no response, retries with the same `claim_request_id` and receives the same
  Attempt via the unique index) or did not (the Target is still `pending`).
- Corrupt manifest: refused, reported as `inconsistent`, never acted on.
- Two workers on one checkout: distinct data dirs are fine (Git serialises worktree
  metadata; each worker only ever removes worktrees under its own root); the same
  data dir is refused by the flock.

## 15. Logging

Logs are for debugging; the journal (§3) is the audit trail, and nothing reads logs to
decide anything. The mechanism is `internal/logging` (`STYLE.md` §9); this section is
the process-level shape.

- **Components.** One per process: `daemon`, `worker`, `mcp`, `plugin.<name>`, and
  the one-shot CLI commands (`cli.<command>`). Inside a process, package loggers
  are dotted (`controlplane.http`, `controlplane.scheduler`, `store`, `worker.git`,
  `worker.supervisor`, `worker.parser`, `tools.<name>`).
- **Sinks.** stderr at the operator's levels (`--log-level`, `--log-format`, `-v`,
  `-vv`; `FORGE_LOG_LEVEL`, `FORGE_LOG_FORMAT`; `[log]` in `<home>/config.toml` and
  `<home>/worker.toml`; flag > env > config > default `info`/`text`). The daemon and worker
  also always write JSON at `debug` to `<forge home>/logs/daemon.log` and
  `<forge home>/logs/worker.log` — one file per process, and there is never more
  than one of either process (daemon lock, worker data-dir lock); rotation by size
  (`max_size_mb`, default 50; `max_files`, default 5 rotated generations). A
  detached process's raw stdio (panics, runtime warnings) goes to
  `<component>.stdio.log`, never to the structured log. `forge mcp`, plugins, and
  one-shot commands have no file sink of their own; a plugin's stderr is captured
  by the daemon into `logs/plugins/<name>.log`.
- **Correlation.** `controlplane.http` middleware generates `request_id` and puts it
  in the request context; the worker's attempt runner builds the attempt context
  with `attempt_id`, `target_id`, `work_id` once, at claim, and every phase and tool
  span adds `span_id`; `forge mcp` gets the attempt from its flag and stamps it on
  everything; plugins get `plugin`. The handler stamps these from the context — call
  sites never repeat them.
- **Runtime control.** `POST /api/v1/log-level {"levels": "debug,store=trace"}` and
  `GET /api/v1/log-level` (`forge daemon log-level X` is the CLI); SIGUSR1 toggles
  debug in any long-running Forge process. Propagation: the daemon signals or
  re-informs the children it started (the worker it spawned), the worker
  reads the daemon's level on every registration and applies it, and `forge mcp`
  reads it at start; every Forge-spawned Forge child also inherits
  `FORGE_LOG_LEVEL`/`FORGE_LOG_FORMAT` from the parent's live handler
  (`logging.Environ`). A child's
  stderr is captured line by line and re-logged under its component (JSON records
  keep their level; anything else is `warn`).
- **Executor output.** stream-json lines and executor stderr are *data*: they go to
  `data_dir/output/<attempt>.log` and the parser. They are mirrored to the log only
  at `trace` (`worker.executor`), never at `info`.
- **Never logged:** prompt bodies, tool output, file contents, tokens, or anything
  from a repository. IDs, names, sizes, durations, decisions, and errors are.

## 16. Milestones and smoke tests

The smoke tests referenced as "smoke N" throughout are listed in `docs/SMOKE.md`,
copied from the build specification and run against the real repositories at the
end of each milestone; the milestone report records the commands and observed
output for each.

The daemon model of §1 was designed in before M1 (decision 2026-08-30, NOTES.md), so
nothing is built to be replaced:

- **M1** model + store (every table, including `journal`, `plugins`,
  `external_refs`) + worker + daemon (`daemon start|stop|status|log-level`, lock,
  `daemon.json`, socket + TCP, handshake, auto-start §1.2, bootstrap §1.3, lease
  extension on start, journal SSE, plain `daemon restart` without drain) + thin
  CLI (`task add|list|show|cancel`, `routine add|list|show|edit|run|enable|disable`,
  `cleanup`, `worker start`, `version`) + facts + basic pages. Smoke 1–10.
- **M2** `forge mcp`, tools, kb, `prune`, prompt versions. Smoke 11–14.
- **M3** budget, queue (`queue list|move|block`), questions (`task answer`), human
  queue, stats, `usage`, `retro` pack. Smoke 15–19.
- **M4** modes, verification, browser tests. Smoke 20–23.
- **M5** reflection (`proposal list|show|approve|reject`). Smoke 24–27.
- **M6** what remains of the daemon work: drain on `daemon restart`, `daemon logs
  -f`, `task logs -f`, `--wait`, `forge init` (interactive; `--service`,
  `--with-browser`), `forge doctor`, `forge service install|uninstall|status`,
  worker capabilities and browser routing. Smoke M6 1–10.
- **M7** plugins (§17) and the Omarchy indicator. Smoke M7 1–8.
- **M8** sandbox (§19: bubblewrap, netproxy allowlist, strace-discovered `claude`
  write paths), `sandbox` capability, fixtures recorded and `just check` proven
  offline. (`fake-claude` itself is M1.) Smoke M8 1–6.
- **M9** write-set leases, `plan` and `integrate` modes, the merge queue and push
  policy (§20; requires the constitution amendment), stacking, conflict-resistant
  conventions applied to Forge, integration facts. Smoke M9 1–8.
- **M10** runners, models, cost vector, router and escalation, prompt overlays,
  capability matrix (§21). Smoke M10 1–6.
- **M11** steer, `notify` plugin, repo briefs, `forge.toml` bootstrap proposals, ask
  budget, retry/dedupe, `curate` mode (§22). Smoke M11.
- **M12** backup/restore, upgrade/rollback, evals, health (§23). Smoke M12.

The detailed specifications for M8–M12 are `forge-m8-plus-prompt.md`; §19–§23 hold
what M1 must already respect. Each milestone extends these sections in place.

## 19. Sandbox (M8)

Executors run under `bubblewrap` by default (`sandbox = true` per executor; the
worker advertises `sandbox: ready|missing`; routines with `require_sandbox = true`,
the default, route only to ready workers). The wrapper is one function,
`Sandbox.Wrap`, applied at launch. Mounts: the worktree read-write at its real path;
the attempt's artifacts/output directory read-write; `<home>/deps` and the executor
binary plus its required config read-only — for `claude`, its config directory
read-only except the session/state paths it must write, discovered with `strace -f
-e trace=file` on a fake run and recorded here in M8; a tmpfs `$HOME` otherwise;
`/usr`, `/etc`, `/lib*` read-only; a private `/tmp`; `--unshare-pid
--die-with-parent --new-session`. Never exposed: `~/.ssh`, `~/.config/gh`,
`<home>/forge.sock`, the worker token, other registered checkouts. Environment: exactly
`PATH`, `HOME` (the tmpfs), `LANG`, `TERM`, `CLAUDE_CONFIG_DIR`, `ANTHROPIC_*`,
`GIT_CONFIG_*` (including `user.name`, `user.email`, `rerere.enabled`, since the
global gitconfig is not mounted), `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`,
`FORGE_HOME`, `FORGE_SOCKET` (the per-attempt socket below), `FORGE_TOKEN` (the
per-attempt MCP token — never the worker token), `FORGE_LOG_*`. Network: `--unshare-net` plus a per-attempt
proxy (`forge netproxy`, in-process in the worker) exposed through a Unix socket
bind-mounted into the sandbox, `HTTP_PROXY`/`HTTPS_PROXY` set, allowlist from
`[sandbox] allow_hosts` in `config.toml`; denied hosts are journaled
(`sandbox.denied`) with the attempt id. `forge mcp` runs **inside** the sandbox (so the repository tools it executes stay
sandboxed) and reaches the daemon through a worker-side per-attempt Unix socket
(`<data_dir>/mcp/<attempt>.sock`, bind-mounted) that forwards only attempt-scoped
routes and rejects everything else; with the per-attempt token that makes tools the
only path to Forge state. The daemon socket is never mounted. Credentials never enter the
sandbox: anything needing `gh` or SSH is a tool executed by Forge outside it.

As built (M8, this machine): `strace` is not installed, so the writable
exception list is the config knob `[sandbox] claude_write_paths` in
worker.toml with the coarse documented default `["~/.claude",
"~/.claude.json"]`; run the strace pass and tighten it when strace is
available. Entries under `~/.ssh` or `~/.config/gh` are rejected at
config load.

## 20. Integration (M9)

**Write sets.** `paths` (globs) come from `task add --paths`, the routine, or `plan`
mode; the scheduler holds them as leases per repository (§10.2). Lockfiles
(`go.mod`, `go.sum`, `package.json`, `*.lock`) are an implicit exclusive lease; a task
declaring `deps` runs a serialized pre-step that adds them as a Forge-authored commit
under the lockfile lease, which goes through the merge queue like any other change
(rebase → checks → push — never a direct push to the integration branch) before the
task starts. Undeclared `paths` means the whole repository.

**Merge queue.** For `integrate = true` Work, `succeeded → queued_for_merge`. One
integrator per repository, serial: rebase the task branch onto the current
integration-branch head (`mergiraf` as merge driver, `merge.conflictstyle=zdiff3`, via
`GIT_CONFIG_*` env); clean → run the repository's declared checks in a fresh worktree
on the rebased result → push the fast-forward (push policy) → journal `merge.pushed`
with before/after SHAs → clean up. Conflict → spawn an `integrate` attempt (conflict
only; no web, no new dependencies; tight budget; must pass L1 and the original
verification level); failure → `conflict`, human queue, worktree retained.
Optimistic batching is out of scope (a future proposal).

**Push policy** — the wording is proposed in the M8 report and lands as the commit
`constitution: gated push for integration` only after approval; until then Forge
never pushes. Forge pushes only from the integrator, only after checks on the actual
merge result, only to a branch listed in the repository's `forge.toml`
(`integration_branch`, optionally `task_branches = "forge/*"`), never with `--force`
(no `--force`, `-f`, or `+refspec` anywhere in a push invocation — a test greps for
them), never deleting remote refs, never from inside a sandbox, via an
`integrate`-only tool executed by Forge with the user's credentials. `main` in the
user's local checkout is never modified; the human pulls.

**Stacking.** If B is `blocked_by` A with `on: success` and `stack_on: true`, B may
start on A's branch head (`stack_base_commit` recorded) before A merges; when A
merges, Forge rebases B onto the new integration head (failure → `integrate`).
`[integration] max_stack_depth` defaults to 2.

## 21. Runners, models, routing (M10)

`config.toml` gains `[runners.<name>]` (`kind ∈ {claude-cli, anthropic,
openai-compatible}`, `billing ∈ {subscription, api, local}`, `capacity`, `endpoint`,
`probe`, `daily_usd_cap`, `usd_per_hour`), `[models.<alias>]` (`runner`, `id`, `class ∈
{frontier, mid, small, local}`, `max_tier`, `executor`, `context`, `price = {input,
output, cache_read, cache_write}` $/MTok), and `[routing]` (`objective`, `weights =
{usd, five_hour, seven_day, runner_seconds}`, `min_verified_success`, `min_samples`,
`explore`). A bundled default config carries the current Anthropic models and prices.
`[runners]` and `[models]` have embedded defaults (`haiku`, `sonnet`, `opus` on
the `claude` runner with current prices); `config.toml` overrides per key and
bootstrap never writes them, so an upgrade refreshes prices. From M1,
`routines.model` and `task add --model` are validated as aliases that exist in
`[models]`, and `attempts.model` stores the resolved id, so M10 changes nothing
about their meaning. The worker probes runners and advertises
`runner:<name> ready|down|unauthenticated`; runner capacity is the second slot
dimension (§10.2). The router scores candidates by the weighted cost vector from
facts (p50 per routine/model, falling back to tier/model), subject to the Wilson
lower bound of verified success ≥ `min_verified_success` once `min_samples` exist
(below that, eligible with probability `explore`); failure escalates to the next rung
with the previous structured result and failing checks in the prompt
(`escalated_from`). Prompt overlays `modes/<mode>.<class>.md` are appended when
present and hashed into the prompt version. `doctor` compares `total_cost_usd` with
tokens × price and flags drift.

## 22. Human loop and knowledge (M11)

`forge task tell ID "…"` injects a user turn (`steer` capability, `--input-format
stream-json`), journaled. A first-party `notify` plugin (`events`) sends desktop
notifications via `notify-send`. `explore` maintains a `brief` note per repository,
injected with `--append-system-prompt`; `tokens_to_first_edit` measures its effect.
A repository without `forge.toml` yields a `doc` proposal for one. Ask budget
(`max_questions`; exceeding it at `ask` fails `ask_budget_exhausted`). `forge task
retry ID`; intake dedupes on `external_refs` and `prompt_hash` within 24 h. `curate`
mode consolidates superseded retro notes monthly.

## 23. Resilience and evals (M12)

`forge backup` (`VACUUM INTO` + kb, modes, prompts, config, plugin state; nightly with
retention) and `forge restore` into a fresh `FORGE_HOME` (tested in `just check`);
migrations back up first; `forge daemon rollback` restores the previous binary and
DB snapshot when a new version fails its post-start self-check; `forge eval` runs
golden tasks under `evals/` and scores verified success, cost vector, and turns — a
`routine`/`mode_prompt` proposal carries an eval score before it can be approved;
`GET /api/v1/health` feeds `doctor` and the status file.

## 17. Plugins (M7)

Integrations live outside the core. The core knows a manifest, a process, a token
with scopes, a journal stream, and MCP aggregation.

A plugin is a directory — first-party under `plugins/<name>/` in the repo (embedded,
installed with `forge plugin install <name>`), third-party under
`<home>/plugins/<name>/` or any directory listed in `config.toml`'s `plugin_dirs`
(so a user keeps out-of-tree customizations in their own repos; `~` and relative
paths resolve at load, earlier root wins a duplicate name, a missing root warns) —
with `plugin.toml`: `name`, `version`, `description`,
`command` (argv, relative to the plugin dir, any language), `capabilities ⊆
{events, tools, intake, annotate}`, `scopes`, `restart ∈ {always, on-failure,
never}`.

- **events** — the plugin consumes `GET /api/v1/journal?since=<cursor>` (SSE). The
  daemon persists the cursor from `POST /api/v1/plugins/<name>/ack` so a restarted
  plugin resumes with no gaps and no duplicates beyond its last ack.
- **tools** — the plugin is an MCP server on its stdio; `forge mcp` aggregates core
  tools with enabled plugins' tools, namespaced `<plugin>_<tool>`, still gated per
  mode by `--allowedTools` and by the plugin's scopes (`tools:provide`). A plugin
  tool call is a span like any other.
- **intake** — the plugin creates Work through the API with `external_refs`; how it
  learns of work (poll, webhook) is its business.
- **annotate** — `external_refs` on Work/Target/Proposal, rendered as links.

Core responsibilities (the complete list): discover and validate manifests;
supervise processes (start on daemon start, restart per `restart` with exponential
backoff, stderr captured into `<home>/logs/plugins/<name>.log` with
`component=plugin.<name>`); mint a per-plugin token carrying only the declared
scopes, shown and approved at `forge plugin enable`; refuse out-of-scope requests
with 403 and a journal row; serve the journal stream with cursors; aggregate MCP
servers; store `external_refs`; expose plugin health on the System page and in
`forge doctor`. `forge plugin list|install|uninstall|enable|disable|logs|status`.
A plugin's environment is exactly `FORGE_SOCKET`, `FORGE_TOKEN`,
`FORGE_PLUGIN_DIR`, `FORGE_LOG_LEVEL`, `FORGE_LOG_FORMAT`, plus the pass-through
list of §1.1 (a deliberate widening of the prompt's "nothing else": without `PATH`
and `HOME` a Python or Node plugin cannot start; recorded in NOTES.md).

Scopes: `events:read`, `work:read`, `work:write`, `usage:read`, `kb:read`,
`kb:write`, `proposal:read`, `tools:provide`, `annotate:write`.

The first plugins are `status-file` (Go, first-party, `events`; maintains
`~/.local/state/forge/status.json` atomically on every relevant journal event and
a 5 s heartbeat; `state` is computed in exactly one function: `throttled` if a
budget hard stop is active, else `attention` if the human queue is non-empty or
anything failed in the last hour, else `working` if any attempt is running, else
`idle`) and `omarchy-indicator` (a Quickshell bar widget + panel installed into
`~/.config/omarchy/plugins/ronin.forge/` that reads the status file; consumers treat
`ts` older than 30 s as "daemon down"). Their full specification is
`forge-m6-m7-prompt.md` §M7; it is copied into `docs/PLUGINS.md` when M7 starts.

## 18. Command tree

Every command is a thin client (§1); each accepts the logging flags (`STYLE.md` §8),
has `--help`, and returns non-zero with a one-line error on bad input. There are no
aliases for older names.

```
forge init | doctor | version
forge task add "<prompt>" [--repo X]... [--mode run] [--routine R] [--priority N]
               [--class interactive|normal|backlog] [--autonomy L]
               [--after WORK_ID]... [--model M] [--paths GLOB]... [--integrate]
               [--wait]
forge task list [--state S] | show ID | logs ID [-f] | cancel ID | answer ID "…"
forge task approve|reject ID ["reason"]          # L3 sign-off
forge task requeue ID                             # conflict → merge queue (M9)
forge task tell ID "…" | retry ID [--model M]     # M11
forge backup [--out DIR] | restore ARCHIVE | eval --mode M [...]   # M12
forge daemon rollback                             # M12
forge routine add|list|show|edit|run|enable|disable NAME
forge queue [list] | queue move ID --before ID | queue block ID --on ID
forge proposal list|show|approve|reject ID
forge usage | stats | retro | prune | cleanup
forge kb new|resolve|backlinks|links|graph|search|check|export
forge daemon start|stop|restart|status|logs|log-level
forge service install|uninstall|status
forge worker start
forge plugin list|install|uninstall|enable|disable|logs|status   # M7
forge mcp --attempt ID                                            # spawned by the agent
```

`task add`: `--repo` is required without `--routine` and optional with it (it
narrows the routine's repositories); the prompt is required without `--routine` and
optional with it (it replaces the routine prompt for this task only). `--class`
defaults to `interactive` for human submissions (§10.3). `task answer` on a task with
one open Question answers it; with several, `--question ID` selects. `--wait`
streams progress (SSE, resuming across restarts) and exits with the task's derived
state (§4.2): 0 `succeeded`/`merged`, 1 `failed`/`partial`/`cancelled`, 3
`unverified`, 4 `waiting_human`/`conflict` when the human is not the caller.

`routine add NAME` takes flags for the common fields (`--mode --prompt --repo… --model
--effort --max-turns --timeout --schedule --autonomy --class --priority`) and
`--from FILE.toml` for everything; `routine edit NAME` opens the routine as TOML in
`$EDITOR` (or applies `--from`) and carries the generation so a stale write is a 409.

Commands that do not auto-start the daemon are listed in §1.2; everything else does.
