# forge (2)

The Forge rebuild, in Rust. One binary, no daemon: the worker is the
long-running process.

```sh
forge run  <repo> "<task>" [--workflow W] [--check CMD]...  # run one task now
forge add  <repo> "<task>" [--workflow W] [--check CMD]...  # queue a task for `forge work`
forge work [--jobs N] [--poll SECS] [--once]                # run queued tasks: stay up and poll, or drain and exit with --once
forge log                                                   # list tasks, newest first
forge retry <id>                                            # re-queue a finished task as a new one: same text, workflow, budget, flags, and dependencies
forge answer <id> "<text>"                                  # answer a task blocked on a question and re-queue it as a retry
forge decisions                                             # list recorded operator answers, newest first
forge show <id>                                             # show one task and its attempts
forge supervise <id>                                        # run the supervisor on a task blocked with a question, now
forge doctor                                                # check this machine can run attempts and nothing is stuck
forge version                                               # print the crate version and, if built from a git checkout, its commit
forge workflows                                             # list the workflows a task can run, with declared metadata and measured outcomes
forge trace <id>                                            # everything about one task: every step's inputs, outputs, verdict rows, and a diagnosis
forge requests                                              # blocked tasks: questions for the operator and workflow requests
forge stats                                                 # outcomes per workflow version and per step
forge events                                                # the event log as JSON lines: a client's subscription
forge snapshot                                              # tasks, requests, the worker, and the event offset to subscribe from, as one JSON object
forge land <id>                                             # land an already-verified task's branch through the integrator: merge the base in, re-verify, push, fast-forward
forge integrate <id>...                                     # merge verified tasks' branches together in order and re-verify after each, without landing
forge journal <id>                                          # what every earlier attempt in a task's piece of work said it did, and what the kernel found
forge gc [--dry-run]                                        # remove worktrees that are clean and whose commits are all on a remote
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
   `--timeout-secs` (default 1800); `--max-turns` (default 100) is the other
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
   and their output (`--retries`, default 1), on the same branch. An
   attempt that hit its turn cap with work in hand is resumed in the same
   CLI session instead of starting over. A run the provider refused for a
   rate window is not an attempt: Forge waits for the reset and goes again.
   A task stops early when its cost reaches its cap. `forge retry <id>` can
   override `--max-turns` and `--timeout-secs` for the new task (default:
   as before). `--max-turns` (default 100) is only a runaway guard against
   a stuck attempt; the cost cap, the `--timeout-secs` wall-clock limit, and
   the provider's rate windows are the real bounds on how much an attempt
   can do.
6. **Land.** On a verified success the kernel brings the base branch in
   as it is now on the remote, re-verifies the merged tree with every
   hidden suite, pushes the branch by explicit refspec, and fast-forwards
   the base; one landing at a time per repository. A conflict or a check
   that fails only with the base merged goes back to the coder as a
   retry. `--no-land` leaves the verified branch pushed for a human.
   See docs/ACTIONS.md, "Landing". A task queued `--after` another is
   claimed only once that one has landed, and is blocked with the reason
   if it fails, so a chain can be queued in one go. A dependency that
   blocks on a question keeps its dependents waiting, and any retry of a
   dependency (`forge retry`, `forge answer`, the supervisor) carries
   them along to the new task. `--after` (and an initiative file's
   `after:` header) may name a task in a different repository: a
   dependency means only "wait for that task to land (or succeed, if it
   never will)", which does not care which repository either task is in.
7. **Record.** Every attempt is a row: agent exit, timeout, turns, tool
   calls, cost from the CLI's accounting, wall time from Forge's clock,
   commits and files from git, and the verdict. The agent's text is stored
   as text.

Forge also ends an attempt itself when two signs of going nowhere trip
at once, as the tool calls stream: thirty calls without an edit, fifteen
edits without a commit, or one command run five times. The session is
kept and resumed with a prompt that names the signs and says what to do
about each, so the work the agent located is not lost and the cap never
has to arrive. Read-only steps (review, investigate) are never faulted
for not editing.

Task states: `queued`, `running`, `succeeded`, `failed` (with the reason),
`unverified` (nothing verified the work, or only the reviewer failed; pushed in the latter case), `blocked` (the agent
asked a question or for another workflow). Attempt states:
`succeeded`, `checks_failed`, `agent_failed`, `unverified`.

`forge retry <id>` re-queues a finished task as a new one: same text,
workflow, budget, and flags, on a fresh branch; everything that had been
queued `--after` it now waits on the new task instead, and anything that
had been blocked by its failure is queued again (`--chain` is accepted
and no longer needed). `forge answer <id> <text>` answers a task blocked on a
question (state `blocked` with its last attempt `needs_input`; anything
else is refused, naming why): it records the question and answer in a
`decisions` table and re-queues the task through the same retry path
(`retry_of` the answered task, same settings and dependencies), with the
task text becoming the original text, a blank line, and "Operator's
answer to a question from an earlier attempt: `<text>`". `forge decisions
[--repo <path>] [--json]` lists every recorded answer, newest first, with
the task id, the question, and the answer. `forge journal <id>` prints what every earlier attempt
in a task's piece of work said it did and what the kernel found; that same
text is fed to each new attempt unless the task was queued `--no-journal`,
the control arm of a measurement. `forge stats` groups outcomes by
workflow hash and by step; `--tools` shows what the agents actually ran
there instead — tool and shell calls with their time, and files read,
aggregated per step.

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

Workflows are lists of actions: directives (an agent step) and operations
(a command, zero dollars). An operation gets the task's facts as
`FORGE_*` environment variables, may commit what it changes (the kernel
verifies the result), and may produce the interface a coder is shown.
See docs/ACTIONS.md.

## Layout

```
src/agent.rs        spawn the CLI, parse stream-json, timeout
src/assess.rs       the assess directive: a read-only score of a landed diff's maintainability
src/attempt.rs      one attempt of a directive: prompt, launch, verdict, the row
src/audit.rs        diagnosis for a terminal failure; cost anti-patterns
src/builtins/       built-in actions, operations, and workflows, as TOML
src/checks.rs       run one command as a check under a timeout
src/cli.rs          commands and all terminal output
src/concierge.rs    forge ask: sorts a customer message into request, question, need, or unclear
src/config.rs       forge.toml and config.toml
src/ctx.rs          Forge: paths, store, budget, sandbox, reporter, built once
src/deploy.rs       forge deploy: run a target's method, record the result, roll back on failure
src/deploy_look.rs  the deploy-look directive: a read-only agent looks at the deployed page
src/directive.rs    one launcher for every bounded agent run, one reading of how it failed
src/doctor.rs       forge doctor
src/engine.rs       run_task / run_attempt, Fault::{Task, Env}
src/envelope.rs     the result contract: schema and parser
src/git.rs          the few git operations Forge performs
src/intake.rs       intake acceptance: a confirmed brief becomes a project
src/job.rs          forge job start: the executor for operation-only run workflows
src/journal.rs      what earlier attempts in a piece of work said, and what the kernel found
src/landing.rs      the integrator: merge base in, re-verify, push, fast-forward
src/main.rs         entry, unix_now
src/operation.rs    a workflow step that is a command, not an agent
src/plugins.rs      plugins: directories named for their plugin.toml, one broken manifest never stops the rest
src/profile.rs      a workflow's measured cost and success, from its runs
src/prompts.rs      what each contract's agent is told, assembled from pieces
src/queue.rs        how a task comes to exist; enqueue validates a TaskRequest
src/render.rs       text rendering for documents: first sentence, path-like tokens stripped, word-boundary cuts
src/report.rs       typed events; the stderr printer is one consumer
src/sandbox.rs      bubblewrap
src/store/          SQLite, forward-only migrations by user_version, one file per table family
  mod.rs            types, column lists, open, schema_version, MIGRATIONS, the migration runner
  tasks.rs          tasks: claim, queue, dependents, lineage
  attempts.rs       attempts and ops: insert, finish, rate limits, tool facts
  jobs.rs           jobs, job_steps, job_effects
  deploys.rs        deploys, deploy_targets, assessments
  projects.rs       projects, project_repos, backlog, initiatives, portal_tokens
  record.rs         decisions, task_refs, plugins
  messages.rs       messages: one row per inbound/outbound message on a channel, so a rule can ask "has this contact replied since"
  webhooks.rs       webhook_tokens: per-hook tokens (only their hashes) that let `forge job fire` start a webhook-triggered job
  stats.rs          forge stats: workflow/step/role/human-attention/time-to-live queries
  stats_tests.rs    stats.rs's #[cfg(test)] mod, split out to keep stats.rs under the line bound
src/supervisor.rs   the rung between a blocked task and the human
src/tools.rs        what an attempt ran, read back from its stream
src/verify.rs       L0/L1/L2, the claim rule, and the pure verdict table
src/view.rs         shapes behind `log`, `requests`, `decisions`: text and JSON from one struct
src/worker.rs       drive, the queue loop, signals
src/workflows.rs    the workflow and action tables, loaded as one Catalog
tests/e2e/          the real binary against fake agents in tests/fakes/

client/   forge-client: the one Rust client of the CLI; typed rows from --json
portal/   forge-portal: read-only project page at /p/<token>, no login
repomap/  forge-repomap: symbol index and task-ranked file list
tui/      forge-tui: the operator's seat, a client of the CLI only
web/      forge-web: the same seat in a browser
```

## Environment

- `FORGE2_HOME` (default `$XDG_DATA_HOME/forge2` or `~/.local/share/forge2`)
  holds `forge.db`, `config.toml`, `worktrees/`, `logs/`. Separate from
  Forge 1's `FORGE_HOME`.
- `FORGE2_CLAUDE_BIN` overrides the agent binary. Anything that accepts the
  same flags and emits stream-json works; the tests use shell scripts.
- `FORGE2_SANDBOX=0` runs the agent and checks directly on the host.
  Without it, missing `bwrap` is an error.

## Building

```sh
cargo build --release
cargo test
```

The e2e suite runs its fakes under the real sandbox, so it requires
`bwrap`; a missing `bwrap` fails the suite loudly rather than silently
skipping sandbox coverage. Set `FORGE2_TEST_NO_SANDBOX=1` to run the
suite unsandboxed on a machine without bubblewrap.

## Where this is going

docs/REVIEW.md is the architectural review of the first week and the
staged refactor plan that follows from it; docs/LATER.md holds ideas
not yet earned.

## The supervisor

When a task blocks with a question, a read-only agent on a strong model
reads the repository's record and answers with citations, files a
prerequisite task, or escalates to you. Every answer is a decision row
whose outcome is the task it re-queued, so `forge decisions` shows which
of its answers landed. See docs/SUPERVISOR.md; `[supervisor]` in the
data-dir config sets the model and the cap on answers per piece of work.

## Clients

`forge-tui` (in `tui/`) is the operator's seat: the queue, the blocked
tasks waiting on a decision with what each agent tried, and a task's
trace with its diagnosis; `r` retries the task under the cursor, `R`
retries it with everything that waited on it. It is a client of the CLI
and nothing else: it reads `log --json`, `requests --json`, and
`trace --json`, acts through `forge retry`, and never opens the database
or links the kernel (`tests/boundary.rs` enforces that). Run it with
`forge` on PATH or `FORGE_BIN` set; `FORGE2_HOME` passes through.
`forge-tui --dump` prints one frame without a terminal.

`forge-web` (in `web/`) is the same seat in a browser: the queue, the open
questions with what each agent tried, a task's trace with its attempts,
diagnosis and journal, and the live event feed. It is the same kind of
client: every route is a forge verb's JSON passed through (`snapshot`,
`log`, `trace`, `journal`), and the feed is `events --follow` as
server-sent events. Read-only for now. It binds `127.0.0.1:7788` unless
told `--bind`, and every request needs the token it generates once into
`FORGE2_HOME/web.token`: it prints the link at start, the first visit
sets a cookie. There are no routes without the token, so a tailnet proxy
in front of it exposes nothing by itself.

Clients never poll. Every event the engine emits is appended as one JSON
line to `events.jsonl` under `FORGE2_HOME`; `forge snapshot` returns the
tasks, the requests, the worker, and the log's byte offset at that
instant, and `forge events --since <offset> --follow` is the subscription
from there. A client applies events to its own state and re-reads a task
only when an event says it changed. See docs/CLIENT.md for the full
contract: every verb a client may call, every JSON document's fields,
every event type, and which listing to re-read on which event.

The worker runs as a user service: `deploy/forge-worker.service`, with
a stop timeout long enough to drain a running attempt. After a rebuild,
`systemctl --user restart forge-worker`; `forge doctor` warns when the
running worker's binary has been rebuilt underneath it.
