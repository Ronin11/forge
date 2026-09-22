# Unattended checks

*2026-09-19. What Forge asks of itself on a schedule, with no operator
watching: the daily doctor, the weekly drift check, the weekly
engineering check, and the weekly economist rebalance. Each is a `kind =
"run"` workflow on the `forge` project's own repository (docs/JOBS.md),
so it is versioned, checked by `forge workflows validate`, and
dry-runnable like any automation.*

## `doctor-daily` — 07:00 daily

One step, `doctor-json-to-effects`: runs `forge doctor --json` and logs a
row effect for every check whose status is `WARN` or `FAIL`. `[assert]`
is that no `FAIL` row was logged; a clean or merely-WARN day is green.
`[skip_if]` reads a marker the action writes on a `FAIL`-free real run
(never on a dry run) — a day already proven clean does not run again.
`[limits] on_failure = "ask:operator"` turns a `FAIL` into a blocked
question; `per_day = 2` allows one retry.

It splices in a second, schedule-free run workflow, `disk-and-logs.toml`
(`{ workflow = "disk-and-logs" }`, docs/JOBS.md, "Steps"), whose own
`disk-and-logs-check` step keeps free space under `FORGE_HOME` above 5 GB
and the logs directory under 2 GB — running `forge gc` on the retained
worktrees when either is breached, and logging a `FAIL` row only if that
is not enough. `disk-and-logs.toml` has no `[trigger] on = "schedule"` of
its own; it fires only as part of `doctor-daily`, or by hand with `forge
job start`.

## `drift-weekly` — Monday 07:30

Four operations, each an action under `.forge/workflows/actions/`:

| step | what it checks |
|---|---|
| `check-claude-cli-version` | the installed `claude` CLI's version against npm's latest `@anthropic-ai/claude-code`, logging both when the installed one is behind |
| `check-model-drift` | a model family's (opus/sonnet/haiku) resolved id this week against last week, read from the `model` field of the `system`/`init` frame (and `message.model` of assistant frames) in attempt logs under `FORGE_HOME/logs` |
| `check-cargo-audit` | `cargo audit` on the workspace, installing `cargo-audit` first if it is missing, one effect per advisory |
| `check-equitizr-freshness` | `https://equitizr.com/api/meta` returns 200, has a snapshot, and its `meta.built_at` (epoch milliseconds) is within 30 days |

`[assert]` is that no effect was logged: a clean week is a green job.
`[limits] on_failure = "ask:operator"` turns any logged effect into a
blocked question naming what moved. The CLI update itself is never run
by the job — `check-claude-cli-version` only logs the exact `npm install
-g @anthropic-ai/claude-code@<version>` command, for the operator to run
by hand at the next worker restart.

## `economist-weekly` — Monday 06:00

One step, `economist-rebalance`: runs `forge economist rebalance`
(docs/ECONOMIST.md, "What is built"), which reads `forge stats
--factors` over the last `ECONOMIST_DAYS` (`[env]`, default 14) and
shifts `experiment.toml`'s weights toward the cheaper, more confidently
measured level of every factor it declares, never below the floor,
writing and committing the result in the workflow catalog's own git with
a message naming what moved. The step (and so the job) fails when a
level's `|effect|` crosses `ECONOMIST_LARGE_EFFECT_THRESHOLD` (`[env]`,
default `1.0`); `[limits] on_failure = "ask:operator"` turns that into a
blocked question naming the effect, so a human sees a large move before
the next week's shift compounds on top of it. Honours `FORGE_DRY_RUN`:
`forge economist rebalance --dry-run` prints the weights it would write
and writes nothing.

## What a repository or task check is told

Not a scheduled check but the other meaning of the word, recorded here
so the two are not confused: a `[checks]` entry in a repository's
`forge.toml` (L1) and a task's own `--check` command (L2) both run with
the task's facts in their environment, `FORGE_TASK_ID`, `FORGE_BASE_SHA`,
`FORGE_START_SHA` and `FORGE_BRANCH`, built by the same function that
gives an operation its environment. docs/ACTIONS.md, "Checks and known
fixes", has the list and what it is for.
