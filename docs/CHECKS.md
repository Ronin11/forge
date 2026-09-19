# Unattended checks

*2026-09-19. What Forge asks of itself on a schedule, with no operator
watching: the daily doctor, the weekly drift check, and the weekly
engineering check. Each is a `kind = "run"` workflow on the `forge`
project's own repository (docs/JOBS.md), so it is versioned, checked by
`forge workflows validate`, and dry-runnable like any automation.*

## `drift-weekly` — Monday 07:30

Four operations, each an action under `.forge/workflows/actions/`:

| step | what it checks |
|---|---|
| `check-claude-cli-version` | the installed `claude` CLI's version against npm's latest `@anthropic-ai/claude-code`, logging both when the installed one is behind |
| `check-model-drift` | a model family's (opus/sonnet/haiku) resolved id this week against last week, read from the `model` field of the `system`/`init` frame (and `message.model` of assistant frames) in attempt logs under `FORGE2_HOME/logs` |
| `check-cargo-audit` | `cargo audit` on the workspace, installing `cargo-audit` first if it is missing, one effect per advisory |
| `check-equitizr-freshness` | `https://equitizr.com/api/meta` returns 200, has a snapshot, and its `meta.built_at` (epoch milliseconds) is within 30 days |

`[assert]` is that no effect was logged: a clean week is a green job.
`[limits] on_failure = "ask:operator"` turns any logged effect into a
blocked question naming what moved. The CLI update itself is never run
by the job — `check-claude-cli-version` only logs the exact `npm install
-g @anthropic-ai/claude-code@<version>` command, for the operator to run
by hand at the next worker restart.
