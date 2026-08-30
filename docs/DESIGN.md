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
(levels L0–L3 and how each is decided).

## 1. Shape

```
                 ┌──────────────────────────────────────────────────────────┐
  browser / CLI  │ control plane   forge serve   127.0.0.1:7340             │
  ──────────────▶│  http api · scheduler · budget · queue · stats · ui      │
                 │  SQLite (~/.forge/forge.sqlite3, WAL)     kb index       │
                 └───────────▲───────────────────────────▲─────────────────┘
                             │ register / claim / heartbeat│ tool calls (http, token)
                             │ events / complete           │
                 ┌───────────┴───────────────┐   ┌─────────┴──────────────────┐
                 │ worker   forge worker     │   │ forge mcp --attempt <id>   │
                 │  repos · git · worktrees  │   │  stdio MCP server, one per │
                 │  manifests · supervisor   │──▶│  agent process; pure http  │
                 │  parser · reconcile       │   │  client of the control     │
                 └───────────┬───────────────┘   │  plane                     │
                             │ exec, Setpgid       └────────────────────────────┘
                     claude --print … (cwd = worktree)
```

`forge run` starts both halves in one process for the single-machine case; they still
talk over loopback HTTP so the boundary is real and `just boundary` can prove the
worker never imports the control plane.

Trust model: one operator, one machine. Worktrees isolate Git state, not hostile code.
The API is loopback-only; worker and MCP routes carry the token from `~/.forge/token`.

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

An optional `forge.toml` *inside* the repository declares:

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
repository belongs to exactly one project; `default` is created by `forge init`.

### Routine

`routines(id, name UNIQUE, mode, prompt, repositories JSON, executor, model, effort,
max_turns, timeout_seconds, max_budget_usd, allowed_tools JSON, autonomy, verification,
priority, budget_class, schedule, schedule_enabled, concurrency, generation,
archived_at)`.
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

### Work

`work(id, routine_id NULL-able, routine_name, generation, trigger, snapshot JSON,
priority, budget_class, autonomy, scheduled_for, submitted_by, created_at,
finished_at)`. `trigger ∈ {manual, schedule, proposal, dependency}`. `snapshot` is the
frozen routine (byte-for-byte what ran). `priority` is mutable (queue reordering);
`finished_at` is written once, at the terminal transition (it bounds the 24 h
attention window); everything else is immutable after creation. Blocked and deferred
reasons are derived at read time (§4.2), never stored. `routine_id` is NULL for Work
Forge creates without a routine (the `verify` Work of §L2); such Work is outside any
routine's `concurrency`.

`work_dependencies(work_id, blocked_by_work_id, on)` with `on ∈ {success, terminal}`.
Inserting an edge that would create a cycle is rejected (`model.WouldCycle` over the
dependency graph of non-terminal Work).

Work state is **derived**, never stored (§4.2).

### Target

`targets(id, work_id, repository_name, state, worker_id, lease_token_hash,
lease_expires_at, cancel_requested, retained, failure_reason, unverified_reason,
claimed_at, started_at, finished_at)`. One per repository in the Work; unique on
`(work_id, repository_name)`. `worker_id` is set at first claim and pins later claims
(resume) to the worker that owns the worktree; a pinned Target whose worker is not
connected stays `pending` with a visible reason (`waiting for worker <name>`) — it is
never failed automatically, a human may cancel it.

### Attempt

`attempts(id, target_id, worker_id, claim_request_id UNIQUE, executor, model, effort,
mode, autonomy, worktree_path, branch, base_branch, base_commit, head_commit, pid,
pid_start, session_id, prompt_version_hash, launches, started_at, finished_at,
exit_code, failure_reason, unverified_reason, is_error, result_text, result JSON, num_turns, input_tokens,
output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd, git_dirty,
git_commits, git_files_changed, git_insertions, git_deletions, git_pushed,
verification_level, verification_passed, cleanup_outcome, cleanup_reason,
cleanup_command, output_path, output_bytes, output_truncated)`.

One Attempt per Target for now; the table allows many. A Target that pauses in
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
created_at)`. §12.

### KbNote, KbLink

`kb_notes(id, path, title, type, created, tags JSON, mtime, hash, body_hash)`,
`kb_links(from_id, to_kind, to_ref, link_type)`, FTS5 `kb_fts(id, title, body)`. §11.

### Worker

`workers(id, name, version, max_concurrent, active, executors JSON, registered_at,
last_seen_at)` and `retained_worktrees(attempt_id, worker_id, path, reason,
cleanup_command)` reported on every registration.

### Journal

`journal(id INTEGER PRIMARY KEY AUTOINCREMENT, ts, kind, entity_type, entity_id,
payload JSON)`. One row per state change of a Work, Target, Attempt, Question, or
Proposal (`entity_type`), written **in the same transaction** as the change by the
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
pending       → claimed | cancelled
claimed       → preparing | failed | cancelled
preparing     → running | failed | cancelled
running       → waiting_human | verifying | failed | cancelled
waiting_human → pending | cancelled
verifying     → succeeded | unverified | cancelled
succeeded, unverified, failed, cancelled → (terminal; no edges)
```

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
- `retained` is a flag, never a state.
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

1. If every Target is terminal: `succeeded` if all succeeded; `unverified` if all are
   succeeded/unverified with ≥ 1 unverified; `cancelled` if all cancelled; `failed` if
   all failed; otherwise `partial`.
2. Else if any Target is `waiting_human` → `waiting_human`.
3. Else if any Target is `claimed`/`preparing`/`running`/`verifying` → `running`.
4. Else if a dependency is unsatisfied (§10.3) → `blocked`.
5. Else if the budget policy currently defers this Work's class → `deferred`.
6. Else `pending`.

Attention items are defined once, in §10.4.

### 4.3 Manifest lifecycle (worker-local)

`preparing → worktree_created → running → exited → cleaned | retained`, with side
states `not_created` (finished before the worktree existed), `inconsistent`
(filesystem and Git disagree; never repaired automatically), and `missing`.
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
(`forge answer`, UI) re-queues the Target as `pending` pinned to the owning worker;
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
control_plane = "http://127.0.0.1:7340"
token_file    = "~/.forge/token"
name          = "laptop"
max_concurrent = 4
data_dir      = "~/.forge/worker"

[executors.claude-code]                # §7.4
command = [...]
output  = "claude-stream-json"
capabilities = [...]

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
`Capabilities()`. The shipped implementation is the template executor above; each
capability the executor declares *and* the routine/mode uses appends flags:

| capability | flags |
|---|---|
| `allowed_tools` | `--allowedTools <comma list>` (Forge tools prefixed `mcp__forge__`) |
| `builtin_tools` | `--tools ""` when the mode disables built-ins |
| `json_schema` | `--json-schema <mode result schema>` |
| `resume` | `--resume <session_id>` (the answer becomes the prompt) |
| `max_budget_usd` | `--max-budget-usd <n>` |
| `effort` | `--effort <level>` |
| `append_system_prompt` | `--append-system-prompt <text>` |

Template variables: `{{model}} {{max_turns}} {{repo}} {{worktree}} {{mcp_config}}
{{session_id}}`. cwd = worktree; prompt on stdin; exit 0 is success; the worker owns
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
seven_day_after, utilization_delta_estimate`).

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

```toml
[budget]
five_hour_target    = 0.90
seven_day_target    = 0.90
five_hour_hard_stop = 0.97
seven_day_hard_stop = 0.97
daily_usd_cap       = 25.0          # optional
[budget.quiet_hours]                # optional, local time
start = "09:00"
end = "18:00"
reserve = 0.20
```

In order, per window (both windows must admit):

1. `u ≥ hard_stop` → defer all classes: `hard_stop:<window>`. Running work is never
   killed (constitution 6). Sum of `cost_usd` since local midnight ≥ `daily_usd_cap`
   → `daily_usd_cap`.
2. `interactive` → admit.
3. Quiet hours: non-interactive classes admit only while `u < target − reserve`
   (`quiet_hours`).
4. `normal` → admit iff `u < target` and the 1 h-rate forecast at reset ≤ `target`
   (`forecast_over_target`). When there are no samples yet, admit.
5. `backlog` → admit iff `normal` would admit **and** `u < target · f`
   (`ahead_of_burn_down_line`). This is burn-down: early in a window the line is low
   and backlog waits; as the reset nears the line rises to the target and backlog is
   admitted aggressively to spend what would otherwise be lost.

Also enforced at claim: routine `concurrency` (Work of that routine with any Target
in `claimed`/`preparing`/`running`; `waiting_human`, `pending`, and `verifying`
Targets do not count, and routine-less Work never counts), worker `max_concurrent`,
and that a connected worker advertises the Target's repository and executor. A Work whose class is deferred shows `deferred` with the reason. Measured:
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
`proposed`; Targets in `verifying` awaiting L3; `notify`-level checkpoint events from
the last 24 h (informational, no acknowledgement — they age out); Work that ended
`partial`/`failed`/`unverified` in the last 24 h; retained worktrees. Each row: what,
from which attempt/routine, waiting since, and the action. `forge answer <question>
"…"`, `forge approve|reject <proposal>`, `forge approve|reject target:<id>`.

## 11. Knowledge base

Markdown notes under `[kb] path` (default `~/.forge/kb`). Frontmatter: `id`
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

Operator (loopback): `GET /api/v1/dashboard`; `GET|POST /api/v1/routines`,
`GET|PUT|DELETE /api/v1/routines/{name}`, `POST /api/v1/routines/{name}/run`;
`GET|POST /api/v1/work`, `GET|PATCH|DELETE /api/v1/work/{id}`; `GET /api/v1/queue`;
`GET /api/v1/attention`; `GET /api/v1/attempts/{id}`, `GET
/api/v1/attempts/{id}/events`; `POST /api/v1/questions/{id}/answer`; `POST
/api/v1/targets/{id}/approve|reject` (L3); `GET|POST
/api/v1/proposals`, `POST /api/v1/proposals/{id}/approve|reject`; `GET /api/v1/stats`;
`GET /api/v1/retro`; `GET /api/v1/usage`; `GET /api/v1/workers`; `GET
/api/v1/repositories`; `GET /api/v1/kb/...`; `GET|POST /api/v1/log-level` (§15).

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
  completion requires the lease token (hash compare). A sweeper (every 10 s, and on
  start) moves expired-lease Targets in `claimed`/`preparing`/`running` to
  `failed:lease_expired` and computes their facts. `pending`, `waiting_human`, and
  `verifying` hold no lease; a `verifying` Target is resolved by its `verify` Work's
  terminal transition or a human, never by the sweeper.
- Worker killed mid-attempt: the lease expires (Target `failed:lease_expired`); the
  restarted worker's reconcile finds the manifest, kills the orphan process group,
  inspects, cleans or retains, and patches the cleanup fields. Nothing leaks (smoke 7).
- Control plane killed mid-attempt: the worker keeps running (heartbeats fail, it
  retries until its own view of the lease expiry passes, then stops the agent as
  `lease_lost`); on restart the sweeper fails the Target; a late completion is
  accepted for its git/cleanup fields (smoke 8).
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
  `-vv`; `FORGE_LOG_LEVEL`, `FORGE_LOG_FORMAT`; `[log]` in `forge.toml` and
  `worker.toml`; flag > env > config > default `info`/`text`). The daemon and worker
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
  re-informs the children it started (the worker under `forge run`), the worker
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

M0 this document and its companions, scaffold. M1 model + store + worker + control
plane + `forge run` + basic pages (smoke 1–10). M2 `forge mcp`, tools, kb, prompt
versions (11–14). M3 budget, queue, questions, human queue, stats, retro pack
(15–19). M4 modes, verification, browser tests (20–23). M5 reflection (24–27).
