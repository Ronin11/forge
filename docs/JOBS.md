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

[assert]
quoted  = ["scripts/assert-quote.sh"]   # exit 0 iff a quote was sent to the sender and logged once

[limits]
budget_usd = 0.10          # per run
per_day    = 200           # runs per day before it asks
on_failure = "ask:contact" # ask:contact | ask:operator | retry:2 | drop
```

- **Trigger.** What starts a job and what it provides as input. The
  worker owns schedules; the channel plugins deliver messages; the web
  clients accept webhooks; Forge's own events (a landing, a deploy) can
  trigger a run workflow on the `forge` project. Every trigger enters
  through one verb, `forge job start`, so a plugin needs nothing new to
  start a job.
- **Steps.** Operations and directives from the catalog, unchanged in
  shape. A directive in a job carries a `role`, routed to a provider
  like every role, and a schema; it is given the step's inputs and its
  instructions, nothing else, and it has no tools. This is the bounded
  judgment the local model is fit for: the harness controls the inputs
  and the assertion checks the output.
- **Effects.** An operation that acts on the world declares its effect
  kind. The executor logs every effect with its target. In a dry run,
  effect operations record what they would have done and do nothing.
- **Assertions.** Commands run after the steps, with the effect log and
  every step's output on disk; exit status is the verdict, as with a
  deploy check.
- **Limits.** A budget per run, a rate per day, and what to do on
  failure.

## The executor

A job is claimed by the worker like a task and runs in a sandbox:

1. Materialise the project's landed tree at its pinned commit into a
   scratch directory (from the repository cache; no clone, no branch).
2. Write the trigger's inputs as files and environment (`FORGE_INPUT_*`,
   `FORGE_INPUT_DIR`), and the project's secrets as environment from the
   operator's store, never into any prompt.
3. Run the steps in order. Outputs are files in the scratch directory
   and flow to the next step the way consumes and produces work today.
   A directive's structured output is validated against its schema
   before the next step sees it.
4. Log each effect as it happens: kind, target, a short description,
   the step that did it.
5. Run the assertions. Record the verdict, the cost, and the step
   timings.
6. On failure, apply `on_failure`: retry, drop, or ask, where asking is
   the human rung: a blocked question on the project, addressed to the
   contact or the operator, carrying the job id and what failed; the
   answer can re-run the job with the answer as an input.

The record: `jobs` (id, project, workflow and its pinned version,
trigger kind and payload reference, started, finished, state, cost,
verdict) and `job_steps` (job, step, provider, model, cost, duration,
output reference) and `job_effects` (job, step, kind, target, summary,
dry_run). `forge job start | list | show | log`, `forge job test` (below),
and the shapes on the client contract.

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
