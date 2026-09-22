# The economist (design note, 2026-09-22)

This is the note the rate-limit write-up in docs/LATER.md refers to,
written down at last, with the numbers the record can now supply. It was
the last item on docs/ROADMAP.md, after the measured week, because the
first thing it needed was a week of undisturbed data. The design below
is still the whole shape of it; "What is built" says how much of that
shape exists today.

## What it is

A pure function, not a model: given a task (its repository, its size,
its workflow if named, its budget) and the state of the world (each
provider's windows, the day's spend, the profiles), it returns three
things: the workflow, the provider per role, and the pace (start now,
hold until a window frees, or start on the cheaper provider now rather
than the better one later). Every input is a row the store already has;
every output is written on the task as its resolved routing, so the
record shows what the economist chose and why, and `forge stats` can
compare its choices with what a fixed routing would have cost.

Two rules Nate set when the idea came up, kept as the spec:

1. **It tunes on true delayed cost, not on landing rate.** A landing is
   not a success until nothing later had to repair it. The number the
   economist minimises is `TRUECOST` from `forge stats --quality`: the
   attempt cost plus the repair cost later attributed to the landed
   lines, per landed task, and it treats human attention (answers,
   hand landings, withdrawals) as cost at a rate the operator sets.
2. **It is a function with a table, never a model call.** Its decisions
   are reproducible from the store and a config, and a decision it made
   can be explained by naming the rows. An operator preference is a
   prior the table can overrule with enough evidence, never a hard
   override, because an override switches the measurement off.

## What the record says today (2026-09-21)

| routing | tasks | landed | cost per landed | attention per landed |
|---|---|---|---|---|
| `direct`, Anthropic | 58 | 78% | $2.08 | 2.11 events |
| `reviewed`, Anthropic | 276 | 68% | $4.74 ($7.93 true) | 0.91 events |
| `cheap` (haiku, 15 turns) | 18 | 22% | $0.88 | |
| `direct`, local model | 43 | 0% | | |
| OpenAI (codex) | 0 | | | |

Read as an economist would: `direct` is the better buy on a repository
with strong checks when someone is around to answer, `reviewed` when
nobody is; `cheap` cannot finish a real chore; the local model takes no
code attempts and keeps the judgment steps; and the OpenAI subscription
has no row at all, which is a gap in the data before it is a gap in the
routing. The night shifts that follow this note exist to fill that row.

## The inputs, and where each lives

- Profiles: `forge workflows` (verified rate with interval, cost per
  verified success, per hash), `forge stats --by-role` (per role and
  provider, attempts and job steps), `--quality` (true cost, repair
  cost, broke-base, churn), human attention and time to live.
- Windows: the rate-limit samples on every attempt (`rl_five_hour`,
  `rl_seven_day`, their resets), the operator's caps, `per_day_usd`.
- Budgets: task, initiative, project defaults.
- Task shape: repository, workflow if named, the text's size, whether
  it names files, whether it has hidden tests.
- The routing record: every task's `routing_json`, per role that ran
  (code, tests, review, plan, assess) the provider, model, and workflow
  it ran under, each with its source (flag, project, operator, default,
  or experiment — the last is piece 4's explore draw). Written where the
  provider is resolved (`src/engine.rs`) and where the model is chosen
  (`src/attempt.rs`); shown by `forge trace` and `forge show`; documented
  in docs/CLIENT.md. This is what makes "provider per role" (below)
  answerable from the record rather than asserted: a decision the
  economist makes can be explained by naming the row that shows why the
  step before it ran where it did.
- Factors: `forge stats --factors [--days N]` (`src/store/stats.rs`,
  documented in docs/CLIENT.md) — over landed and failed tasks in the
  window, per factor level (provider per role, workflow, task size in
  three bins) the task count, the landing rate with a Wilson 95%
  interval, and the mean true cost per landed task; plus one joint
  least-squares fit of log true cost across every level at once, each
  reported against its factor's reference level with a standard error.
  What decisions 2–4 below read before they choose anything: an interval
  that does not clear the alternative's is not a decision yet, only a
  level with too few tasks under it.

## The decisions, in the order to build them

1. **Pace.** The one decision already done by hand: use the window or
   lose it. Given the reset time and the queue, either hold or spend;
   when a provider's window is near its cap and another provider's is
   not, route the next attempt there. Needs the OpenAI row.
2. **Provider per role.** For each role (code, tests, review, plan,
   assess, supervisor) the provider whose true cost per verified
   success is lowest with an interval that clears the alternative's,
   else the operator's prior. The supervisor stays on its own model by
   design.
3. **Workflow.** `direct` against `reviewed` by repository and task size,
   with attention priced in.
4. **Task size.** The one lever docs/LATER.md's capped-attempt note
   found: a task over a size threshold costs five times the mean; the
   economist can refuse to route it and ask for a split, which the
   `planned` workflow already does.

## What is built

Four pieces, in the order they landed:

1. **Task shape**, recorded on every task at intake and never revisited:
   `shape_text_len`, `shape_path_tokens`, `shape_tdd`,
   `shape_declared_checks` (`queue::task_shape`). What "Task size" (below)
   and `forge stats --factors`' `"size"` factor condition on.
2. **The routing record.** `ctx::resolve_provider_routed` names, per role,
   which layer chose its provider — `"flag"` (the task's own
   `--provider`), `"experiment"` (an explore draw, this section),
   `"project"`, `"operator"`, or `"default"` — written to `Task::routing`
   as each step runs (`engine::run_directive_step`) and shown by `forge
   trace`/`forge show`. `Task::explore` (`queue::assign_explore`) is the
   per-role draw itself, from the operator's `[measure] explore` config;
   piece 4 (below) adds a second source of draws onto the same field.
3. **`forge stats --factors [--days N]`** (`Store::factor_stats`,
   `src/store/stats.rs`): over landed and failed tasks in the window, per
   level of every factor (`"provider:<role>"` for each role in
   `store::ROLES`, `"workflow"`, `"size"`) the task count, the landing
   rate with a Wilson 95% interval, the mean true cost per landed task,
   and — from one joint least-squares fit of `ln(true cost)` across every
   level at once (`fit_main_effects`, normal equations by hand, no
   library) — the level's effect against its factor's reference level
   with a standard error. `--json` is the same doc `forge stats`'s own
   `StatsDoc.factors` carries.
4. **Randomized assignment within declared bounds, and its weekly
   rebalance** (`src/experiment.rs`). The rest of this section is piece 4.

### `experiment.toml`

Beside the workflow catalog (`workflows::catalog_dir`, the same
git-backed directory `forge workflows put` writes into) an operator may
declare `experiment.toml`:

```toml
floor = 0.1               # optional; this is the default

[factors.review]
anthropic = 0.7
openai = 0.3

[factors.code]
anthropic = 1.0
```

Each `[factors.<role>]` table names a role in `store::ROLES` (`code`,
`tests`, `review`, `plan`, `assess`) and, per level (a provider name), a
raw weight; `experiment::load` normalizes each factor's weights to sum to
1.0 and refuses a file (naming the file, the factor, and the level) whose
normalized weight falls under `floor` — a level under the floor cannot be
measured again, so it is a config mistake caught at load rather than a
level quietly starved to nothing. The file is excluded by name from the
workflow parser (`workflows::toml_files`), so it never counts as a broken
workflow and never blocks task creation the way any other malformed file
in the catalog would.

### The draw

`queue::enqueue` draws a level per factor for a task that names no
`--provider` of its own **and** resolves its workflow from the built-in
default (no `--workflow`, no project default) — a task or project that
already named either shows deliberate intent, not the default routing the
experiment measures. A factor whose role a project's own `[roles]` table
pins is skipped entirely: the experiment only assigns within declared
bounds, never against a pin. The draw (`experiment::draw_level`) is a
pure, splitmix64-style function of the task id and the factor's own name,
so every factor of one task draws independently of every other (unlike
`journal_control_draw`'s single shared draw tested against several
fractions) and the same task always redraws the same level. A drawn level
is merged into `Task::explore` — the exact field and CLI/JSON surface
`[measure] explore` already used — so it flows through
`ctx::resolve_provider_routed` and lands on `Task::routing` with source
`"experiment"`, with no change needed to how a step resolves its
provider or how the record is read.

### The weekly rebalance

`forge economist rebalance [--days 14] [--threshold 1.0] [--dry-run]`
(`.forge/workflows/economist-weekly.toml`, Monday 06:00 UTC in this
repository's own catalog) reads `Store::factor_stats` over the window —
the same numbers `forge stats --factors --json` prints — and, per factor
`experiment.toml` declares, shifts each level's weight
(`experiment::rebalance`) by a signal built from that level's `effect`
and `effect_se` against its factor's reference level: positive (toward
more weight) when the level is cheaper and the fit is confident, negative
when costlier, zero for the reference level itself or a level the fit
dropped for too little data. The shift is multiplicative and capped
(`LEARNING_RATE`, `MAX_SIGNAL` in `src/experiment.rs`) — a fraction of a
swing per week, never a jump — then renormalized and floored
(`experiment::apply_floor`, a water-filling normalization: raise every
level under the floor to it, shrink the rest proportionally, repeat until
nothing more needs raising) so no level ever goes under the floor.

A level whose `|effect|` clears `--threshold` is a large move: the
*whole factor it belongs to* is held back (`experiment::rebalance`'s
`held`) — its weights stay exactly as `experiment.toml` already has them,
nothing computed for it is written, so a move large enough to ask about
never lands in the catalog on its own. Every other factor is rebalanced
and, unless `--dry-run`, written to `experiment.toml` and committed in the
catalog's own git (`git::commit_path`) with a message naming every level
that moved and its before/after weight (`experiment::commit_message`);
`--dry-run` only prints what it would have written. Either way, the
command exits non-zero when any level crossed the threshold, dry run or
not, naming which level and why, and — for each held factor — the
proposal it computed but did not write, as the `forge experiment set`
invocation that would apply it by hand (`experiment::large_move_message`).
A real (non-dry) run's `[limits] on_failure = "ask:operator"` turns that
non-zero exit into a blocked "job question" task carrying that same
message — the human rung, so a human sees a large move, and the numbers
it would have shifted to, before the next week's shift compounds on top
of it. A dry run's exit code is informational only: the job driver never
honours `on_failure` for a dry run, so a fixture replay (`forge job test`)
can never file a real question.

`forge experiment set <factor> <level>=<weight>...` (`experiment::set_factor`)
applies a held factor's proposal, or any other weight, by hand: the same
validation `experiment::load` applies to every factor in the file (role
known, positive weights, normalized, none under the floor), then writes
and commits `experiment.toml` exactly like a normal rebalance.

## Subscriptions: price at list, spend by the window (2026-09-22)

The first real rebalance (job 26) proposed moving OpenAI up on both
roles by a wide margin. The fit was honest and the input was wrong: the
subscription reports tokens but no dollars, so 146 OpenAI attempts sat
in the record at $0 and the economist saw a free provider. Two rules
follow, and both are now in place.

- **Price every provider at its API list**, whatever the operator pays.
  Cost in the record is a measure of the work's weight, used to compare
  quality per dollar across providers; `[providers.<name>]` carries the
  prices and `forge stats --reprice` fills in rows that were recorded
  before they were set.
- **A subscription's marginal cost is zero until its window closes, and
  that belongs to the pace decision, not the cost.** The economist's
  first decision (pace) is where "use the OpenAI window while it is
  open" lives; its cost comparison never sees a zero.

And one rule of conduct that the same run taught: a rebalance whose
effect crosses the threshold **asks before it writes**. The factors it
would move by more than the threshold keep their weights, the question
carries the proposed numbers, and `forge experiment set` applies them
when a person agrees.

## What it is not

Not a learning system, not a scheduler, not a spend cap (those exist).
Not something that changes what verification means: it chooses who does
the work and when, never what counts as done.
