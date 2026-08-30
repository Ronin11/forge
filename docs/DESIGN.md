# Forge V1 design

Forge runs Claude Code against local Git checkouts, on demand or on a cron schedule.
One Go binary, one SQLite file, one machine. It is a scoped rewrite of Factory
`561e883`; the invariants below are the ones carried over.

## Data model

| Record | Owner | Purpose |
| --- | --- | --- |
| **Repository** | worker | `[repositories.<name>]` in `worker.toml`: a non-bare checkout with an `origin`. The worker validates it, pins its origin identity, and advertises it on registration. The control plane never clones. |
| **Routine** | control plane | Saved procedure: `name`, `prompt` (`{{repo}}` allowed), `repositories`, `executor` (default `claude-code`), `model`, `max_turns`, `timeout_seconds`, `allowed_tools`, optional `schedule` + `enabled`, `concurrency` (max active Work, default 1). `generation` increments on every edit. |
| **Work** | control plane | One invocation (`manual` or `schedule`) with a frozen copy of prompt and settings and the routine generation. State is *derived* from its Targets. |
| **Target** | control plane | One repository within one Work. Holds the state machine, lease, cancel flag, and `retained` flag. |
| **Attempt** | both | One execution of a Target on one worker: worktree path, branch, base commit, pid, timestamps, exit code, result text, tokens/cost, git outcome, cleanup outcome + reason + command. One per Target in V1; the table allows many. |
| **Event** | both | Bounded per-attempt log lines (lifecycle, stdout summaries, stderr). Cap: 2000 events / 1 MiB per attempt; the raw stream goes to `data_dir/output/<attempt>.log`. |
| **Worker** | control plane | Registered worker: name, slots, advertised repositories + executors, retained worktrees, `last_seen_at`. |

Invariants (from Factory): a Work snapshot never changes after admission; edits to a
Routine only bump `generation`; a Work is terminal only when every Target is terminal;
workers only ever receive the frozen Target snapshot; the control plane only routes a
Target to a worker that currently advertises its repository and executor.

## Target state machine

```
pending ──▶ claimed ──▶ preparing ──▶ running ──▶ succeeded
   │           │            │            │
   │           │            │            ├──────▶ failed
   └───────────┴────────────┴────────────┴──────▶ cancelled
```

`model.Transition(from, to)` is the only function that changes a Target's state; every
other edge is rejected. `retained` is a flag on the Target/Attempt, never a state.
Work state precedence: all terminal → `succeeded` / `cancelled` / `failed` / `partial`;
otherwise `running` if any Target is claimed/preparing/running or terminal Targets
coexist with pending ones; otherwise `pending`.

Leases: a claim starts a 30 s lease; heartbeats (every 10 s) renew it and return the
cancel flag. A sweeper moves expired-lease Targets to `failed` with reason
`lease_expired` — never back to `pending`. `DELETE /api/v1/work/{id}` cancels pending
Targets immediately and sets `cancel_requested` on active ones; the worker kills the
process group on its next heartbeat. Concurrency: a new Work for a Routine is not
admitted (scheduled: skipped; manual: 409) while `concurrency` Works are active.
Missed schedule ticks while the server was down are skipped.

## Attempt sequence (worker)

1. Claim → Attempt row created, Target `claimed`.
2. `git fetch --no-tags origin <base>`; resolve `origin/<base>` to a full commit. Verify
   the checkout's origin still matches the pinned identity before and after.
3. `git worktree add -b forge/<routine-slug>-<attempt8> data_dir/worktrees/<repo>/<attempt> <commit>`.
   The checkout's working tree, index, and HEAD are never touched.
4. Write the manifest `data_dir/attempts/<attempt>.json` (0600, temp+fsync+rename)
   **before** launching anything. Heartbeat `preparing`.
5. Launch the executor: cwd = worktree, prompt on stdin, `Setpgid`, deadline = routine
   timeout, stdout parsed line by line by the named parser, stderr tailed, both mirrored
   to the raw output file with bounded size. Record pid + process start identity in the
   manifest. Heartbeat `running`.
6. Batch events to the control plane. Timeout / cancel / shutdown → SIGTERM the process
   group, wait 5 s, SIGKILL.
7. On exit inspect Git: dirty tree? commits beyond base? are they on any `refs/remotes`?
8. Cleanup decision (below), then `complete` with result, usage, git outcome, cleanup.

## Cleanup rules

| Worktree state | Action | Recorded reason |
| --- | --- | --- |
| path missing or not registered | nothing to remove | `worktree missing` |
| dirty (`git status --porcelain` non-empty) | retain | `dirty worktree` |
| commits beyond base, none on a remote ref | retain | `unpushed commits` |
| clean, HEAD == base or HEAD contained in a remote ref | `git worktree remove` (no force) | `removed` |
| removal failed | retain | the git error |

The branch is never deleted. The registered checkout is never modified. Retained
attempts record the exact `forge cleanup <attempt> --confirm` command, which force-removes
the worktree but keeps the branch.

## Reconcile

Manifests are the worker's source of truth for "what did I start". On start and every
five minutes the worker loads every non-final manifest that is not an attempt running
in this process: if the recorded pid is alive with the recorded start identity it is
killed (process group); then the Git state is inspected, the cleanup rules applied, and
the outcome reported with `complete` (state `failed`, reason `worker_restart`). If the
control plane already closed the Target (lease expired) only the cleanup fields are
updated. Manifests end in `cleaned` or `retained`.

## Executor contract

```toml
[executors.claude-code]
command = ["claude", "--print", "--verbose", "--output-format", "stream-json",
           "--dangerously-skip-permissions", "--model", "{{model}}", "--max-turns", "{{max_turns}}"]
output = "claude-stream-json"   # or "lines"
allowed_tools_flag = "--allowedTools"   # optional capability
```

Template variables: `{{model}}`, `{{max_turns}}`, `{{repo}}`, `{{worktree}}`. Exit 0 is
success. A `Parser` consumes stdout lines, returns a one-line summary per line for the
event log, and yields a `Result{Text, NumTurns, Usage, CostUSD, IsError}`. Parsers are
registered by name in `worker/parser.go`; a new executor is one config entry (and one
file if it needs a new parser).
