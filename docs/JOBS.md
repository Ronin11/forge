# Jobs: workflows that run instead of build

*2026-09-17. The product is a workflow too. This note says what changes
when a workflow's run produces an effect instead of a landing, and what
is deliberately the same.*

## The observation

A customer's automation is a workflow in Forge's own sense: a sequence
of steps, most of them deterministic scripts, one or two of them an
agent making a bounded judgment; inputs, outputs, a record, a budget,
and failures that should reach a person as a question rather than a
crash. "When a customer texts a photo, extract the job, draft a quote,
send it, log it" is four steps in the vocabulary the catalog already
has: operations for scripts, directives for agents, what each consumes
and produces. And because there is an agent in the mix, the run is
subject to token windows, budgets and provider routing, which is the
machinery Forge exists to manage. The runtime for a customer's
automation should therefore be Forge, not a program thrown over a wall.

What differs is what a run produces. A run of a build workflow is a
**task**: it starts from a clone, makes a branch, is verified by checks,
and ends in a landing. A run of a run workflow is a **job**: it starts
from a trigger, produces an effect, and ends when the effect is
verified. No repository, no branch, no landing; many runs a day; cheap;
verified per run.

That is one flag on the definition and one new kind of work item.
Everything else, the catalog, the record, projects and budgets,
providers and roles, the escalation ladder, the portal, is shared.

## Vocabulary

- **Workflow kind.** A workflow file declares `kind = "build"` (the
  default, and every workflow that exists today) or `kind = "run"`.
- **Job.** One run of a `run` workflow: its trigger, its steps, the
  effects it performed, its verdict, its cost. Jobs live beside tasks in
  the record and on the portal.
- **Automation.** A `run` workflow with a trigger, on a project. "The
  quote-by-text automation" is a workflow file in that project's
  repository plus the scripts it calls.
- **Effect.** A side effect on the world an operation performs: a
  message sent, a row written, a file produced, an HTTP call made.
  Effects are declared, allowlisted, logged, and replaceable by a dry
  run.

Internal versus external is not a flag. It falls out of ownership: a
run workflow on a customer's project is their automation; one on the
`forge` project is Forge automating its own operations, which we will
want too.

## Where an automation lives

In the project's repository, like everything Forge builds:

```
.forge/workflows/quote-by-text.toml     the run workflow
.forge/workflows/actions/*.toml         actions the built-in catalog does not have
.forge/fixtures/quote-by-text/*.json    recorded inputs with expected effects
scripts/…                               what the operations call
```

The build workflows edit these. A task on the project that changes the
automation is verified the way every task is, and the landing is the
new version. Deploying an automation is landing it: the worker runs the
next job at the project's latest landed commit. The deploy target for
an automation is Forge itself, implicit, unless an effect has to happen
on a machine Forge does not run on (below).

But a repository's own checks never ran the catalog's loader against
its own workflow files, so a broken one could land clean: twice,
equitizr's `.forge/workflows/publish-snapshot.toml` landed in a shape
the loader cannot parse (a string `trigger` instead of a `[trigger]`
table, `[[steps]]` tables with an inline `run` command, an `http_get`
step carrying invented `url` and `field` keys) because its reviewer had
no way to run the parser. `forge workflows validate [<path>]` (default:
the current directory) is that check: it loads every
`.forge/workflows/*.toml` and `.forge/workflows/actions/*.toml` under
the path with the same parser the operator's catalog uses, checks a run
workflow's `[trigger]`, that every step names an action that exists
(the repository's own or a built-in), and that `effect` is set only on
an operation step, and prints every problem with its file and, when the
parser can place it, the line — `n workflow(s), m action(s) valid` and
exit 0 when there are none, non-zero otherwise. It opens no store and
needs no FORGE2_HOME, so it runs as an ordinary repository check (wired
into `forge.toml`'s `[checks]`, or a CI step) on any host that has the
`forge` binary, the same way `cargo test` or `eslint` does.

## The definition

A run workflow adds three sections to the format that exists:

```toml
name = "quote-by-text"
kind = "run"
description = "a customer texts a photo of a job; they get a quote back and it goes in the book"


steps = [
  { action = "extract-job",  role = "read" },      # directive: photo + text → job description (schema)
  { action = "price-job" },                         # operation: rules from the price sheet
  { action = "draft-quote",  role = "write" },     # directive: description + price → a text (schema)
  { action = "send-quote",   effect = "message" },  # operation: Signal to the sender
  { action = "log-quote",    effect = "row" },      # operation: append to the book
]

[trigger]
on = "message"            # message | schedule | webhook | event | manual
contact = "customers"     # the Signal plugin's contact group that starts it
# delay = "5m"             # optional: wait this long after the event before the job is due (see "Delayed jobs")

[assert]
quoted  = ["scripts/assert-quote.sh"]   # exit 0 iff a quote was sent to the sender and logged once

[skip_if]
already_quoted = ["scripts/skip-if-already-quoted.sh"]  # exit 0 to skip this run; exit 1 to proceed

[limits]
budget_usd = 0.10          # per run
per_day    = 200           # real starts in 24 hours before the next is refused
on_failure = "ask:contact" # ask:contact | ask:operator | retry:2 | drop (parsed; honoured at step 5)
```

- **Trigger.** What starts a job and what it provides as input. The
  worker owns schedules; the channel plugins deliver messages; the web
  clients accept webhooks; Forge's own events (a landing, a deploy) can
  trigger a run workflow on the `forge` project. Every trigger enters
  through one verb, `forge job start`, so a plugin needs nothing new to
  start a job.

  A schedule is the worker's own trigger: every pass of `forge work`'s
  poll loop ticks it, once, before it fills a free slot. For every
  project, it resolves every run workflow that names it — the project's
  own `.forge/workflows/*.toml` at its latest landed commit, and the
  operator's catalog, by name, the same repository-first-then-catalog
  order `forge job start` uses (above) — and keeps the ones with
  `[trigger] on = "schedule"`. `cron` is parsed with `croner`
  (`Cargo.toml`) at workflow load time, the same moment an unknown `on`
  value is refused: an expression `croner` cannot parse is a load error,
  with the file and the line, before any job ever tries to run it. For
  each schedule, the tick finds the latest cron occurrence at or before
  now and compares it to the workflow's last scheduled job — the most
  recent `jobs` row with `trigger_kind = "schedule"` and this project and
  workflow, its `trigger_ref` the slot as a unix second
  (`store::last_scheduled_job`). A slot newer than that starts one job,
  queued (never run inline), with `trigger_kind = "schedule"` and
  `trigger_ref` set to the slot; a slot already recorded does not start
  again, so ticking twice inside the same minute, or restarting the
  worker, cannot double-fire it (`jobs_schedule_slot`, a unique index on
  `(project, workflow, trigger_ref)` where `trigger_kind = 'schedule'`,
  backs this even if two ticks ever raced). A gap while the worker was
  down — several slots missed — still starts exactly one job, for the
  latest missed slot: catch-up never replays every slot in between. The
  decision itself is pure (`worker::due_schedules`): given now, a set of
  schedules and each one's last slot, it returns what is due and at
  which slot, with no I/O — the tick around it does the resolving,
  reading and writing.
- **Steps.** Operations and directives from the catalog, unchanged in
  shape. A directive in a job carries a `role`, routed to a provider
  like every role, and a schema; it is given the step's inputs and its
  instructions, nothing else, and it has no tools. This is the bounded
  judgment the local model is fit for: the harness controls the inputs
  and the assertion checks the output.

  Because a directive has no tools, it needs no agent CLI either: a
  provider whose `runner` is `"chat"` (`agent::Runner::Chat`) answers a
  directive with a single call to an OpenAI-compatible
  `/chat/completions` endpoint instead of spawning `claude` or `codex`.
  The endpoint is named by `base_url` (ollama's
  `http://dev.home:11434/v1` for a local model, `https://api.openai.com/v1`
  for OpenAI), the step's instructions (the untrusted-data sentence
  every Forge prompt carries, plus the action's own description and
  `prompt`) go as the system message and the step's inputs as the user
  message, and `model` picks what runs. The request asks the endpoint to
  hold the model to the step's schema itself
  (`response_format: {"type": "json_schema", ...}`); an endpoint that
  does not understand that parameter gets a second request instead, with
  no `response_format` and the schema quoted in the system message
  asking for the JSON object alone — either way the answer is validated
  against the schema before it is trusted, so a fallback that ignores
  the instruction fails the step rather than passing through unchecked
  output. Tokens and cost come from the response's own `usage` and the
  provider's `price_usd_per_million_input/output` (see
  "Agent backends" in the operator's `config.toml`). A `runner = "chat"`
  provider is refused for anything but a job's directive step — a
  task's code step still has to act, so it still needs an agent CLI.
- **Effects.** An operation that acts on the world declares its effect
  kind. The executor logs every effect with its target. In a dry run,
  effect operations record what they would have done and do nothing.
- **Assertions.** Commands run after the steps, with the effect log and
  every step's output on disk; exit status is the verdict, as with a
  deploy check.
- **`skip_if`.** Named commands, checked like `[assert]` but before any
  step runs instead of after (see "Skipping a run", below).
- **Limits.** A budget per run, a rate per day, and what to do on
  failure.
- **`[env]`.** A table of extra environment for every operation step and
  `[skip_if]` command of this workflow's jobs: a threshold, a table name, a
  URL — a fact the automation's own author picked, not one the kernel
  knows. Declaring `RUN_TASK_MAX_LINES = "400"` here instead of writing
  `400` into the script means changing the number is a workflow-file edit,
  not a script edit. Refused on a build workflow, like `[assert]` and
  `[skip_if]`.

## The executor

A job is claimed by the worker like a task and runs in a sandbox:

1. Materialise the project's landed tree at its pinned commit into a
   scratch directory (from the repository cache; no clone, no branch).
2. Write the trigger's inputs as files and environment (`FORGE_INPUT_*`,
   `FORGE_INPUT_DIR`), the workflow's own `[env]` table, and the project's
   secrets as environment from the operator's store, never into any
   prompt. `FORGE_PROJECT` and `FORGE_REPO_DIR` name the project and its
   real, landed repository (unlike the scratch directory a step runs in,
   this one has a `.git`) for a step that has to act on the project
   itself, such as filing a task with `forge add`; `FORGE_BIN_DIR` and
   `FORGE2_HOME` are where that `forge` binary and the operator's own
   store live, so a step's own recursive `forge` call reaches the same
   store this one did, not a default.
3. Run `[skip_if]`, in name order. The first command to exit 0 ends the
   job right here — see "Skipping a run", below — before step 4 ever
   runs.
4. Run the steps in order. Outputs are files in the scratch directory
   and flow to the next step the way consumes and produces work today.
   A directive's structured output is validated against its schema
   before the next step sees it.
5. Log each effect as it happens: kind, target, a short description,
   the step that did it.
6. Run the assertions. Record the verdict, the cost, and the step
   timings.
7. On failure, apply `on_failure`: retry, drop, or ask, where asking is
   the human rung: a blocked question on the project, addressed to the
   contact or the operator, carrying the job id and what failed; the
   answer can re-run the job with the answer as an input.

## Skipping a run

Most automations are "fire, check the record, act or skip": a rule that
decides there is nothing to do this time — the quote was already sent,
the row is already there — is not a failure, and treating it as one
would turn every "already handled" firing into a red row. `[skip_if]` is
the third outcome:

```toml
[skip_if]
already_quoted = ["scripts/skip-if-already-quoted.sh"]
```

Named commands, in the same shape as `[assert]`, run in the scratch tree
with the job's environment (`FORGE_JOB_ID`, `FORGE_INPUT_*`,
`FORGE_INPUT_DIR`, the project's secrets) before any step. They are
checked in name order; the first to exit 0 ends the job in a new state,
`JobState::Skipped`, with the first line of its stdout as the reason —
nothing further runs, so no step executes and no effect happens. A
non-zero exit means "not skipped, proceed": once every `[skip_if]`
command has said so (or there are none), the steps run exactly as they
would with no `[skip_if]` at all.

A skip counts against nothing: not the workflow's `per_day` rate (a real
start that turns out to have nothing to do should not use up the day's
budget), never touches `on_failure` (a skip is not a failure to retry,
drop, or ask about), and never adds to the failed rollup. It is still
recorded as an ordinary `jobs` row — `forge job list`/`show` and the
portal show it like any other run, with its reason — and rollups
(`forge project show`, `forge stats`) count it separately from `ok`,
`failed`, and `needs_human`.

`forge workflows validate` parses `[skip_if]` the same way it parses
`[assert]`: a build workflow (the default `kind`) may not have one.

The record: `jobs` (id, project, workflow and its pinned version,
trigger kind and payload reference, started, finished, state, cost,
verdict, due time) and `job_steps` (job, step, provider, model, cost,
duration, output reference) and `job_effects` (job, step, kind, target,
summary, dry_run). `forge job start | list | show | log | withdraw`,
`forge job test` (below), and the shapes on the client contract.

## Delayed jobs

A job can wait before it is due, rather than being claimed the moment it
is created. This is a row, not an in-memory timer: `jobs.due_at`, a unix
second, and one new state, `JobState::Scheduled`, that a job with a due
time sits in until then. `claim_next_job`'s one query already claims the
oldest `queued` job; it now also claims the oldest `scheduled` one whose
`due_at` has passed, in the same statement. Nothing runs early, nothing
is missed on a restart: the wait is `due_at <= now`, checked fresh every
time the worker asks, never a sleeping task holding state in memory.

Two ways to get there:

- **`forge job start --at <unix>` or `--delay <duration>`.** A manual
  start that should wait: `--at` names the due second directly, `--delay`
  names how long from now (`s`, `m`, `h`, `d`, e.g. `5m`, `1h`, `2d`).
  Refused together with `--now`, which runs inline immediately instead.
  If the computed due time is already past — `--delay 0s`, or `--at` in
  the past — the job is `queued` right away rather than `scheduled`; the
  next `claim_next_job` treats it exactly as any other queued job.
- **`[trigger] delay = "5m"`.** A trigger that should not fire the moment
  its event happens. Parsed at workflow load time with the same duration
  grammar, refused with the file and the line the way an invalid `cron`
  is. When a firing has a delay, the job it creates is `Scheduled`, with
  `due_at` the firing's own event time (a schedule's due slot, not the
  moment the tick happened to run) plus the delay — so a worker that was
  briefly down still computes the same due time it would have live.

A scheduled job is visible in `forge job list`/`show` with its due time,
and is **withdrawable**: `forge job withdraw <id>` drops it before it
ever becomes due, the job analogue of withdrawing a queued task. It is
refused once the job is no longer `scheduled` — claimed, run, or already
withdrawn — the same atomic-on-state guard a task's withdraw uses, so a
withdraw racing the worker's own claim never wins against a job already
in flight.

## Verifying an automation

This is the part that makes automations buildable by Forge rather than
merely runnable. An automation is verified by **replaying its fixtures
through the run workflow in dry-run mode and comparing the effect log
to the expectation** each fixture records. That is `forge job test
<workflow>`, and it is the repository's check for automation code:
deterministic where the steps are deterministic, and for directive
steps either a recorded output (the fixture pins what the model said
last time) or a live model with an assertion loose enough to survive
rewording. A build task that changes the automation cannot land unless
every fixture still produces its expected effects.

The tests contract works for automations as for code: the tests agent
writes fixtures and expectations it has seen fail on the base; the
coder makes them pass; neither sees the other's work. And the reviewer
and the assessor read the automation like any diff.

## The human rung, per run

Every job that fails its assertions and is set to ask produces one
question, on the project, in plain words, to the contact or the
operator, with what it was trying to do and what did not happen. The
answer re-runs the job with the answer available as an input, or drops
it. The supervisor stays out of questions addressed to a person, as it
does now. The daily question cap from intake applies per contact, so a
misbehaving automation cannot flood someone's phone; past the cap it
holds and the operator hears.

## The portal

An automation is what "Running for you" was always going to list: its
name, its trigger in words, when it last ran, how many runs today, how
many needed someone, and each run's effects as one line ("quoted the
Hendersons' fence job at $1,240"). The job record is the source; the
portal renders it in the customer's words and hides everything else.

## Effects that must happen elsewhere

Most effects are messages, rows and HTTP calls Forge can perform from
its own host. Some must happen on a machine Forge does not run on: a
file on someone's laptop, a printer, a program that only exists there.
For those, a **local runner** plugin: a small process on that machine
that speaks the CLI over the network, executes operations tagged
`where = "local:<name>"` in its own sandbox, and reports back. It is
the plugin model applied to effects, and it is the last piece, not the
first.

## Security posture

Trigger payloads are untrusted, and every directive's prompt says so,
as every Forge prompt does. A directive in a job has no tools. Effects
are allowlisted twice: the workflow declares which kinds it performs,
and the project declares which targets are allowed (which contacts may
be messaged, which hosts may be called). A run has a budget; a trigger
has a rate; a failure asks a person rather than retrying forever.
Secrets are per project and reach only the operation that needs them.
Tenancy isolation is the same open item it is for tasks (`docs/GTM.md`,
item 1) and a job runs in the same sandbox a task does until then.

## What is deliberately absent

No flow editor; the file is the definition and the portal shows it in
words. No general event bus; triggers are the five kinds named. No
long-lived stateful jobs; a job is one pass, and state lives in the
project's own data, owned by its operations. No agent decides what an
automation does at run time; the steps are fixed, the judgments are
bounded, and the assertions are the truth.

## Build order

Each step is an initiative on the `forge` project, sized to land.

1. **The kind and the record.** `kind = "run"`, `[trigger]`, `[assert]`,
   `[limits]`, `effect` on operations; `jobs`, `job_steps`,
   `job_effects`; `forge job start` (manual trigger, inputs from a
   file), `list`, `show`, `log`; the executor for operation-only
   workflows with dry-run and the effect log; the shapes on the client
   contract. First user: a run workflow on the `forge` project with no
   agent in it, such as "export and publish equitizr's snapshot".
2. **Directive steps.** Bounded prompts with inputs and a schema, roles
   routed to providers, the per-run budget, and the local model
   measured on a real judgment step.
3. **Triggers.** Schedules in the worker; the message trigger through
   the Signal plugin; webhooks on the web client and the portal; Forge
   events. Rates per trigger.
4. **Verification.** Fixtures and expectations, `forge job test`, the
   repository check, the tests contract for automations.
5. **The human rung.** `on_failure`, questions carrying a job id, answers
   that re-run, the daily cap.
6. **The portal.** Automations in Running for you with runs and effects
   in the customer's words.
7. **The first customer automation**, built by Forge from a confirmed
   brief: the plumber's quote-by-text, or Nate's own example, end to
   end, as the bench for everything above.
8. **The local runner**, when a real effect needs it.
