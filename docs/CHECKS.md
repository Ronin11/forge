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

A third step, `gc-worktree-retention`, runs `forge gc --older-than
GC_OLDER_THAN_DAYS` (default 7): a retained worktree whose task finished
at least that many days ago is removed even when unpublished, after its
branch is pushed (to the remote when the repo has one, else into the
registered repository) so nothing is lost. It logs the bytes freed under
`FORGE_HOME/worktrees` as a row effect (121 GB was retained there on
2026-09-22, mostly build directories, before this step existed).

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

## The doctor between daily runs, and its tasks

`doctor-daily` is the daily record, not the only run. The worker runs the
doctor watches (`src/doctor/watch/`) at the end of every poll pass, so a
condition is seen within one poll rather than at 07:00. What a standing
ERROR (or a WARN that names a concrete repair) owes is a task in the
`forge` project, opened once per watch while the condition stands and
marked handled when the watch goes OK; the table of watches, the keys and
the outside dead-man (`[alerts] deadman_url`, an `ExecStopPost` on the
worker unit) are in docs/OPS.md, "What doctor watches". A task opened this
way is an ordinary task: its checks are the repository's own, and its text
is what Forge wrote, never a model's.

## What a repository or task check is told

Not a scheduled check but the other meaning of the word, recorded here
so the two are not confused: a `[checks]` entry in a repository's
`forge.toml` (L1) and a task's own `--check` command (L2) both run with
the task's facts in their environment, `FORGE_TASK_ID`, `FORGE_BASE_SHA`,
`FORGE_START_SHA` and `FORGE_BRANCH`, built by the same function that
gives an operation its environment. docs/ACTIONS.md, "Checks and known
fixes", has the list and what it is for.

## The failing-test parser

A check's `failing_tests` field is filled by the `forge-test` crate
(`forge-test/src/lib.rs`), which has no dependency on the kernel and ships
as the `forge-test` binary in `FORGE_BIN_DIR`; `src/checks.rs` calls its
library. `forge-test failing < output` prints the names, one per line.
It reads the merged output tail and recognises, per line:

- go test: `--- FAIL: TestName (0.00s)`;
- pytest: `FAILED path::test - reason`;
- jest: `✕ name` or `✗ name`;
- cargo test: `test foo::bar ... FAILED`, the indented name list under
  `failures:`, `---- foo::bar stdout ----`, and
  `thread 'foo::bar' panicked at` (the `main` and unnamed threads are not
  tests). Several test binaries in one run accumulate; a compile error has
  no test lines and names nothing.

Names appear once, in order of first appearance. Output in no known format
yields an empty list rather than a guess.

## L0: `no-stray-files`

Fails when the attempt's net additions match `*.bak`, `*.backup`,
`*.orig`, `*.rej`, `*~`, `temp_*`, `tmp_*`, or `scratch_*`. Patterns
match filenames at any directory depth, including rename destinations.
The check compares the attempt's starting commit with HEAD; existing
files are not rejected for being modified, and a stray file added and
deleted within the same attempt passes. Uncommitted files are handled
by `clean-tree`. Failure details name the stray files and tell the agent
to delete them and commit the removal.

## Timing: a loaded host makes checks slower, never failed

A check's timeout (`check_timeout_secs`) is a ceiling for an idle host. When
a check starts, the kernel reads the 1-minute load average per CPU and runs
the check under `check_timeout_secs * scale`, where `scale =
max(1, loadavg1 / cpus)`, so a host at load 32 on 8 CPUs gives every check
4x the time instead of timing out on tests that all pass. The e2e harness
multiplies its deadlines by the same function (`src/load.rs`, included by
`tests/e2e/support.rs`), so the two cannot disagree; `FORGE_TEST_TIME_SCALE`
pins the factor for both.

The factor is capped by `[checks] max_time_scale` (default 4) so a runaway
host cannot hold a slot forever; a check that still outruns its scaled
timeout fails as timed out. The verdict row of each check records `scale`
and `timeout_ms` (the timeout it actually ran under), and the attempt log
gets a note naming both. The `[forge] timed out after Ns` line states the
scaled timeout.

## Timing in tests

*2026-10-10. On that day eleven Forge tasks failed their `test` check on
tests that each pass alone in under a second to a few seconds: the host's
load average was 30-45 with several tasks' suites running at once. Every
one of them was waiting on a clock instead of on an event.*

**Wait on the event, with a generous deadline.** A test says what it is
waiting for (a row in the store, a line in a log, a file a fake agent or
plugin writes, a process exiting) and polls for that, with a deadline that
is only the ceiling on how long a broken run may hang. Never assume the
event arrives "within 5 s", and never make a fake's own lifetime a fixed
sleep the test must beat: gate it on a file the test writes once it has
looked (`gated-ok.sh`, `feedbackcoder.sh` with `FAKE_GATE`, the dying
successor in `successor.rs` waiting for `successor-exit`). A reset time
that must still be in the future when the worker looks (`ratelimited.sh`'s
`FAKE_RESET_SECS`) is scaled the same way as a deadline.

**Scale every deadline.** `FORGE_TEST_TIME_SCALE` is the factor deadlines
are multiplied by, never below 1: `support::scaled(Duration)` and
`wait_until` in the e2e suite, `crate::test_time::scaled` in unit tests.
Set, it is used as given (`1` turns scaling off). Unset, it is the 1-minute
load average divided by the CPU count when that is above 1, read once per
test binary, so a loaded host waits longer rather than failing. It scales
deadlines, never pauses a test is meant to make.

**Bounded concurrency.** The e2e harness holds at most as many `Env`s alive
at once as the host has free CPUs (CPU count minus load average, never
fewer than half the CPUs), whatever `--test-threads` says; a test building
several `Env`s counts once.

**Seeing flakiness.** Each e2e test appends one line to
`target/tmp/e2e-timing.jsonl` when its `Env` drops: `test`, `secs`, `load`
at start, `scale`, `passed`, `at`. `cargo test --test flake -- --nocapture`
lists the tests whose duration varied more than 5x over their last 20
runs (ignoring runs under half a second), with the load at the slowest
one. It prints and never fails. `forge doctor` has a `flaky tests` row:
WARN when, in the last 24 hours, a `test` check failed and the supervisor's
mechanic found its failing tests passing again on the base, untouched by
the branch (its `mechanic-load-flake` decisions, docs/SUPERVISOR.md).

**Not allowed:** `#[ignore]`, and retrying a test until it is green. Both
hide the fault. A test that varies more than 5x in the report has a
timing assumption to find.
