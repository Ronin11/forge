# forge (2)

The Forge rebuild, in Rust. One binary, no daemon.

```sh
forge run  <repo> "<task>"   # do one task now
forge add  <repo> "<task>"   # queue it
forge work                   # drain the queue, one task at a time
forge log                    # tasks, newest first
forge show <id>              # one task and its attempts
forge gc                     # remove worktrees that are safe to remove
```

## What a task is

A task is a repo path and a sentence. Forge:

1. Reads `forge.toml` in the repo: a `[checks]` table of argv arrays and an
   optional `[defaults]` with `base_branch`, `remote` (default `origin`),
   and `push` (default `true`).
2. Adds a worktree on branch `forge/<id>-<slug>` from the base branch. The
   registered checkout is never touched beyond `worktree add`.
3. Runs `claude --print --output-format stream-json` in the worktree under
   bubblewrap: read-only system, private `/tmp`, `/run`, `/proc`, a tmpfs
   `$HOME` holding only the worktree, the repo's `.git`, the agent binary,
   and the claude CLI's own state. Network is shared. Nothing else on the
   host is visible. Git identity is passed in as `GIT_CONFIG_*`.
4. Counts commits and changed files with git, then re-runs every declared
   check in the same sandbox. The agent's word is never the verdict.
5. If a check fails, starts another attempt on the same branch with the
   failing checks' output in the prompt (`--retries`, default 1). If the
   agent crashes or hits `--max-turns`, the next attempt is told to
   continue economically.
6. On success, pushes the branch to the remote by explicit refspec, never
   forced, never the base branch. GitHub remotes get a compare URL.
7. Records every attempt in SQLite. Every number comes from git, the CLI's
   own accounting, or Forge's clock. The agent's final text is stored as
   text. Each attempt's raw stream is its log, prompt first.

Task states: `queued`, `running`, `succeeded`, `failed` (with a reason),
`unverified` (the repo declared no checks; nothing is pushed). Attempt
states: `succeeded`, `checks_failed`, `agent_failed`, `unverified`.

## Running unattended

`forge work` claims the oldest queued task, runs it, and repeats until the
queue is empty, then exits. Run it from a timer. A task that errors
internally is recorded as failed with the error as its reason and the loop
moves on. Tasks left `running` by a worker that died are requeued on the
next start and resume at the following attempt number.

Budgets live in `<FORGE2_HOME>/config.toml`, written with defaults on first
use:

```toml
[budget]
per_task_usd = 2.0    # a task stops retrying once its attempts have cost this much
per_day_usd = 20.0    # no new task is claimed once the last 24 hours cost this much
```

`--budget` overrides the task cap for one task. A running attempt is never
killed by the budget; `--max-turns` is its cliff.

`forge gc` removes a worktree only when it is clean and every commit it
added is on a remote (or it added none). Everything else is kept with the
reason and the command a human would run. Branches are never deleted.

## Environment

- `FORGE2_HOME` (default `$XDG_DATA_HOME/forge2` or `~/.local/share/forge2`)
  holds `forge.db`, `config.toml`, `worktrees/`, and `logs/`. Separate from
  Forge 1's `FORGE_HOME`.
- `FORGE2_CLAUDE_BIN` overrides the agent binary. Any program that accepts
  the same flags and emits stream-json works; the smoke tests use shell
  scripts.
- `FORGE2_SANDBOX=0` runs the agent and checks directly on the host.
  Without it, missing `bwrap` is an error.

## Deliberately absent

No daemon, no sockets, no web UI, no merge queue, no GitHub issue source,
no supervision, no personas, no plugins, no learning loop, no multiple
runners. Each is added only when a real run demonstrates the need.

```sh
cargo build --release
cargo test
```
