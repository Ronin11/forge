# The economist (design note, 2026-09-22)

Not built. This is the note the rate-limit write-up in docs/LATER.md
refers to, written down at last, with the numbers the record can now
supply. It is the last item on docs/ROADMAP.md, after the measured week,
because the first thing it needs is a week of undisturbed data.

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

## What it is not

Not a learning system, not a scheduler, not a spend cap (those exist).
Not something that changes what verification means: it chooses who does
the work and when, never what counts as done.
