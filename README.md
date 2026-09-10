# forge (2)

The Forge rebuild, in Rust. One binary, no daemon: the worker is the
long-running process.

```sh
forge run  <repo> "<task>" [--workflow W] [--check CMD]...   # do one task now
forge add  <repo> "<task>" [--workflow W] [--check CMD]...   # queue it
forge workflows                               # the workflows a task can run
forge work [--jobs N] [--poll SECS] [--once]  # run the queue and stay up
forge log                                     # tasks, newest first
forge show <id>                               # one task, its attempts, every check
forge gc [--dry-run]                          # remove worktrees that are safe to remove
forge doctor                                  # can this machine run attempts; is anything stuck
```

## What happens to a task

1. **Create.** `forge.toml` in the repo declares `[checks]` (argv arrays) and
   optional `[defaults]`: `base_branch`, `remote` (default `origin`), `push`
   (default `true`), `check_timeout_secs` (default 600). A task may add its
   own acceptance commands with `--check`. A task nothing would verify, no
   repo checks and no `--check`, is refused at creation.
2. **Clone.** A single-branch clone of the base branch on `forge/<id>-<slug>`,
   with no remote: the agent inside cannot fetch anything, and the
   registered checkout and its `.git` are never mounted in the sandbox.
   Forge pushes by URL. The checks are then read from the base commit,
   never from the branch under test.
3. **Agent.** `claude --print --output-format stream-json --json-schema …`
   in the worktree under bubblewrap: read-only system, private `/tmp` `/run`
   `/proc`, a tmpfs `$HOME` holding only the worktree, the repo's `.git`,
   the agent binary, and the claude CLI's state. Network shared. Killed at
   `--timeout-secs` (default 1800); `--max-turns` (default 30) is the other
   cliff. The raw stream is the attempt's log, prompt first.

   The schema makes the agent's final result structured, never parsed out
   of prose: `summary`, `changes[]` (every path touched), `checks_run[]`
   (only checks it actually ran, with the real outcome), `claims[]` each
   with evidence, and `needs_input` when it cannot proceed without the
   operator. All of it is a claim; step 4 compares it with what Forge
   measured. The stream's rate-limit frames (five-hour and seven-day
   utilization) are recorded per attempt: on subscription billing that,
   not the notional dollar figure, is the real budget.
4. **Verify.** Three levels, each run by Forge after the agent exits, each
   a row in the attempt's verdict. A level runs only if the one before it
   passed.
   - **L0** consistency with git: a structured result exists, clean tree,
     at least one commit, `forge.toml` untouched, the reported `changes[]`
     match what git saw in both directions, every claim has evidence. A
     `needs_input` question ends the task here with the question as its
     reason; retrying cannot answer it.
   - **L1** the repo's declared checks, in the sandbox, each under
     `check_timeout_secs`. Then the claim rule, one-directional: a check
     the agent reported as passed that Forge could not reproduce is a
     `claim:<name>` row that fails. The agent may be conservative, never
     optimistic. Failing test names are extracted from go test, pytest,
     and jest output and fed to the retry by name.
   - **L2** the task's `--check` commands, same treatment.

   A check's exit decides it. Its process group is killed after it exits
   and the output drained with a short grace, so a check that backgrounds
   a server cannot hold the attempt open.
5. **Retry.** On failure the next attempt is told exactly which rows failed
   and their output (`--retries`, default 1), on the same branch. A task
   stops early when its cost reaches its cap.
6. **Push.** On a verified success the branch is pushed by explicit refspec,
   never forced, never the base branch. GitHub remotes get a compare URL.
7. **Record.** Every attempt is a row: agent exit, timeout, turns, tool
   calls, cost from the CLI's accounting, wall time from Forge's clock,
   commits and files from git, and the verdict. The agent's text is stored
   as text.

Task states: `queued`, `running`, `succeeded`, `failed` (with the reason),
`unverified` (nothing verified the work; not pushed), `blocked` (the agent
asked a question or for another workflow). Attempt states:
`succeeded`, `checks_failed`, `agent_failed`, `unverified`.

## The worker

`forge work` claims the oldest queued task, runs it, and repeats. `--jobs N`
runs N at once with output prefixed by task id. When the queue is empty it
polls every `--poll` seconds (default 30); `--once` drains and exits, for a
timer. Every error is classified: a task fault fails that task with the
reason and the loop continues; an environment fault (no disk, no bwrap, a
broken store) puts the task back in the queue and stops the worker, so a
broken machine never marks a queue of tasks failed. Tasks left `running`
by a worker that died are requeued at the next start and resume at the
following attempt number.

Signals: the first SIGINT or SIGTERM stops claiming and lets running
attempts finish. A second aborts them, the sandbox tree dies with the
child, and their tasks go back in the queue.

Budgets live in `<FORGE2_HOME>/config.toml`, written with defaults on first
use. `per_task_usd` stops a task's retries; `per_day_usd` stops the worker
claiming once the rolling 24-hour spend reaches it. `--budget` overrides
the task cap for one task.

The same file's `[sandbox]` section lists what the sandbox exposes beyond
the attempt's own holes. `ro_paths` (default `~/.local/share/mise`) are
toolchains bound read-only, since `$HOME` is otherwise empty in there and
a node or cargo installed under it would be invisible. `rw_paths` (default
`~/.npm`, `~/.cargo/registry`, `~/.cargo/git`) are package caches bound
read-write and shared across attempts; lockfile integrity is what makes
that safe. Paths that do not exist are skipped.

A check named `setup` runs before the others and gates them: if it fails,
nothing else runs. Its outputs (`node_modules`, `target`) must be
gitignored, or the next attempt fails L0 for a dirty tree.

## Layout

```
src/main.rs     entry, unix_now
src/cli.rs      commands and all terminal output
src/ctx.rs      Forge: paths, store, budget, sandbox, reporter, built once
src/engine.rs   run_task / run_attempt, Fault::{Task, Env}
src/verify.rs   L0/L1/L2, the claim rule, and the pure verdict table
src/envelope.rs the result contract: schema and parser
src/doctor.rs   forge doctor
src/worker.rs   drive, the queue loop, signals
src/workflows.rs the workflow table
src/agent.rs    spawn the CLI, parse stream-json, timeout
src/checks.rs   run one command as a check under a timeout
src/sandbox.rs  bubblewrap
src/git.rs      the few git operations Forge performs
src/store.rs    SQLite, forward-only migrations by user_version
src/config.rs   forge.toml and config.toml
src/report.rs   typed events; the stderr printer is one consumer
tests/e2e.rs    the real binary against fake agents in tests/fakes/
```

## Environment

- `FORGE2_HOME` (default `$XDG_DATA_HOME/forge2` or `~/.local/share/forge2`)
  holds `forge.db`, `config.toml`, `worktrees/`, `logs/`. Separate from
  Forge 1's `FORGE_HOME`.
- `FORGE2_CLAUDE_BIN` overrides the agent binary. Anything that accepts the
  same flags and emits stream-json works; the tests use shell scripts.
- `FORGE2_SANDBOX=0` runs the agent and checks directly on the host.
  Without it, missing `bwrap` is an error.

## Deliberately absent

No web UI, no merge queue, no GitHub issue source, no L2 by an independent
agent session, no human sign-off queue, no answering of `needs_input`
questions (the question is recorded; the task ends), no personas, no
plugins, no learning loop. Each is added only when a real run demonstrates
the need.

```sh
cargo build --release
cargo test
```
