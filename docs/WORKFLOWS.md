# Workflows

Decided 2026-09-10. Heavy-handed on purpose; back off later if the data
says so.

## The rule

Every task runs a workflow. There is no mode where a model is handed a task
and runs until done. A workflow is an ordered list of agent steps; after
every agent step the kernel verifies, and after the last one the kernel
pushes and, later, integrates. Verification and integration are not steps a
workflow lists, because they are not optional.

The model's autonomy is the inside of one step. Retries, stopping,
escalation, and what counts as done are the workflow's and the kernel's,
from tables.

Steps are bounded by capability, not instruction. The coder cannot run
verification, push, integrate, or see the verification namespace, because
the sandbox does not give it those things. That is what makes the workflow
unbypassable, and therefore what puts every workflow on the hot path where
it accumulates numbers.

## Why

Forge 1 had workflows and they were second-class to the driving loop: a
big model could just do the thing, so nothing called them, so nothing
iterated on them. Optional next to a capable model means dead. The fix is
not nicer workflows; it is no bypass.

## The workflows

Data, not code: one TOML file per workflow in `<FORGE2_HOME>/workflows/`,
written with the built-ins on first use and edited by the operator.

```toml
name = "tdd"
description = "one agent writes hidden tests that fail on base; another makes them pass"
steps = [
  { action = "tests" },
  { action = "setup" },
  { action = "code" },
]
```

A step references an action (a directive or an operation, one file each
under `workflows/actions/`, see docs/ACTIONS.md) or another workflow,
spliced inline, plus optional `model`, `max_turns`, and `timeout_secs`
overriding the action's own defaults and the task's. A `[meta]`
table declares what the author knows for a human or an agent choosing a
workflow: `use_when`, `avoid_when`, `requires`. A top-level `assess =
true` (default false) opts the workflow into running the `assess`
directive once after a landing on it, scoring the landed diff's
maintainability rather than sitting in `steps` like every other action
(see docs/ACTIONS.md, "Assessment"); of the built-ins, `reviewed` and
`tdd` set it. Never a cost: cost and
success are measured from runs, and a workflow with fewer than five runs
is `unknown`. `forge workflows` shows, per current version and per
previous version over the last fifty tasks, the verified success rate
with its 95% interval, cost per task and per verified success, time, and
attempts, plus the cost ratio to `direct` once both are known. A version
whose success interval falls entirely below the previous version's is
flagged as a regression, in `forge workflows` and in `forge doctor`. That
lookback is the comparator applied to workflows: a change to a file is
judged by the numbers that follow it, and reverting is git. No conditionals,
no variables. A workflow's identity is its name plus a content hash of the
file; every task records the hash it ran under, so two versions of "tdd"
are never averaged together. Git versions the file; the hash pins it. Not
markdown, not a diagram: a diagram can be generated from this for display
but is never parsed to execute. SQLite later means registering name and
hash the first time a task uses one.

The set of step kinds is closed and small:

- `code`: an agent in a per-task clone of the base branch, writing the
  change. May not touch protected paths or the verification namespace.
- `tests`: an agent in its own clone, writing tests only inside the
  verification namespace. The tests must fail on the base commit (red)
  and be committed to `verify/<id>`. Its summary is the interface handed
  to the coder.

See docs/ACTIONS.md for every built-in workflow, its steps in order, and
what each action does.

Kernel steps, always, not listed:

- verify: L0 on the step's tree; then the verification namespace is
  overlaid from the trusted refs (`forge-verify` for standing suites,
  `verify/<id>` for the task's tests) and L1 and L2 run; then the overlay
  is removed so the next attempt starts blind.
- push: the verified branch, by explicit refspec.

What happens after the last step is landing: integrate the base, re-verify,
push, fast-forward. See docs/ACTIONS.md, "Landing".

## Jobs: `kind = "run"`

A workflow file declares `kind = "build"` (the default, everything above)
or `kind = "run"`. A run workflow is an automation: it starts from a
trigger, its steps perform effects on the world instead of writing to a
branch, and it ends in a verified effect instead of a landing. No
repository, no branch, no landing; many runs a day; cheap; verified per
run. See docs/JOBS.md for the full picture (jobs, the executor, fixture
verification); this is the shape of the file alone.

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
contact = "customers"     # the contact whose messages start it; "*" for anyone

[assert]
quoted  = ["scripts/assert-quote.sh"]   # exit 0 iff a quote was sent to the sender and logged once

[skip_if]
already_quoted = ["scripts/skip-if-already-quoted.sh"]  # exit 0 to skip this run; exit 1 to proceed

[limits]
budget_usd = 0.10          # per run
per_day    = 200           # real starts in 24 hours before the next is refused
on_failure = "ask:contact" # ask:contact | ask:operator | retry:2 | drop (parsed; honoured at step 5)
```

(TOML binds a bare `key = value` to the nearest preceding `[table]`
header, so `steps` has to come before `[trigger]`, not after it, to be
the workflow's own field rather than `trigger.steps`.)

A run workflow adds four sections and two step fields to the shape
above, unchanged otherwise: a step still names an `action` (or another
workflow, spliced inline), with the same `model`, `max_turns`, and
`timeout_secs` overrides.

- **`[trigger]`.** What starts a job. `on` is one of `manual`,
  `schedule`, `message`, `webhook`, `event`, and takes exactly one more
  field naming what it triggers on: `schedule` a `cron` expression,
  `message` a `contact` (a name, or `"*"`), `webhook` a `name`, `event` a Forge
  event `type`; `manual` takes none. A `cron` expression is evaluated in
  UTC — `0 7 * * *` fires at 07:00 UTC, whatever zone the operator or the
  worker's machine is in; time is UTC everywhere in the kernel and the
  record, and a zone is a rendering concern of each client. `cron` is
  parsed (with `croner`) at load time, the same as an unknown `on`: an expression that cannot
  parse is refused with the file and the line before the workflow loads
  at all. The worker's poll loop fires a schedule; recording an inbound
  message (`forge message record --from`) fires a `message` trigger whose
  `contact` is `"*"` or the message's own contact, queuing one job per
  message with `{"from", "text", "at", "channel", "message_id"}` as its
  input — `FORGE_INPUT_TEXT` is the text (docs/JOBS.md, "Triggers").
  A `webhook` trigger's `name` is what a caller fires: `forge job fire
  <project> --webhook <name> --token <token>`, or the web client's `POST
  /hooks/<project>/<name>` (docs/CLIENT.md), queues one job with the
  request body (a JSON object) as its input, once per delivery key — the
  caller's `--ref`, else a hash of the body — behind a token minted with
  `forge project webhook token <project> <name>`. A name is letters,
  digits, `-`, `_` and `.`, and one run workflow per project should claim
  it: a hook two workflows claim is refused (docs/JOBS.md, "Triggers").
  An `event` trigger's `type` is a Forge event type from `src/report.rs`
  (`task_done`, `deploy_finished`, `job_finished`, ...), refused at load
  time when it is not one. The worker's poll loop reads the event log past
  the offset this workflow last examined and queues one job for each such
  event that belongs to its project — a task, a deploy or a job of it —
  with the event's JSON as its input (`FORGE_INPUT_STATE` is a
  `task_done`'s `state`), once per event, so a restart starts none twice.
  A job's own `job_finished` never starts the workflow that produced it
  (docs/JOBS.md, "Triggers").
- **`[skip_if]`.** Named commands, each a list like an action's `run`,
  checked in the scratch tree with the job's environment before any
  step — before `[assert]`, before any effect happens. The first to
  exit 0 ends the job in a new state, `Skipped`, with the first line of
  its stdout as the reason; a non-zero exit means "not skipped,
  proceed". A skip counts against nothing — not `per_day`, not
  `on_failure`, not the failed rollup — and is recorded as an ordinary
  job row like any other outcome (docs/JOBS.md, "Skipping a run").
- **`[assert]`.** Named commands, each a list like an action's `run`,
  checked after the steps with the effect log and every step's output
  on disk; exit status is the verdict.
- **`[limits]`.** `budget_usd` (per run); `per_day`, the number of
  real (not dry) starts allowed in any 24 hours, past which `forge job
  start` refuses with a reason naming the limit (asking instead is
  docs/JOBS.md step 5); and `on_failure`: `ask:contact`, `ask:operator`,
  `retry:N`, or `drop`, validated today and honoured when step 5 lands.
- **A step's `role`.** A directive step in a job carries a `role`
  instead of running the kernel's fixed contracts; it is routed to a
  provider like every role (docs/CONFIG.md), given the step's inputs
  and instructions, and has no tools.
- **A step's `effect`.** An operation step that acts on the world
  declares its effect kind: `message`, `row`, `file`, or `http`. The
  executor logs every effect with its target; a dry run records what it
  would have done and does nothing.

A build workflow (the default `kind`) may not have `[trigger]`,
`[skip_if]`, `[assert]`, or `[limits]` — those are a run workflow's
sections. A run workflow needs `[trigger]` and at least one step, the
same "at least one step" every workflow needs. An unknown `on`,
`effect`, or `on_failure` value is refused with the file and line, the
same as any other malformed field. Loading, hashing, and versioning are
otherwise identical to a build workflow: `forge workflows` marks a run
workflow and shows its trigger in words.

### Where an automation lives

A run workflow that belongs to a project lives in that project's own
repository, at `.forge/workflows/<name>.toml`, with any actions the
built-in catalog does not have under `.forge/workflows/actions/`
(docs/JOBS.md, "Where an automation lives"). Nothing in the repository's
own checks used to validate those files against the real format, so a
shape the loader cannot parse — a string `trigger`, a `[[steps]]` table
with an inline `run` command, an invented step field — could land
clean. `forge workflows validate [<path>]` (default: the current
directory) is the fix: it loads every `.forge/workflows/*.toml` and
`.forge/workflows/actions/*.toml` under the path with the catalog's own
parser, checks a run workflow's trigger, that its steps each resolve to
a real action, and that `effect` is set on an operation step only, and
prints every problem with its file and line where the parser can place
one. It opens no store and needs no FORGE2_HOME, so it runs as a plain
repository check, in `forge.toml`'s `[checks]` or in CI, on any host
that has the `forge` binary.

### Fixtures, and `forge job test`

Whether a run workflow still does what it should is the other repository
check. Its fixtures live beside it, at `.forge/fixtures/<workflow>/*.json`:
an `input`, an `expect` (the state the run ends in and the effects it must
log, no more and no fewer), and optionally `outputs`, a recorded structured
output per directive step standing in for its model call
(docs/JOBS.md, "Verifying an automation", has the shape).
`forge job test [<workflow>] [<path>]` replays every fixture of the named
workflow, or of every run workflow in the repository, through the executor
in dry-run mode, in a scratch directory, without recording a job and
without needing FORGE2_HOME, and prints each fixture as `pass` or by its
first difference: a missing effect, an extra effect, a wrong state. It
exits 1 on any difference, so a repository that holds automations lists it
in its own `forge.toml` (which Forge never edits for you), next to
`forge workflows validate` if it wants both:

```toml
[checks]
workflows = ["forge", "workflows", "validate"]
job-test  = ["forge", "job", "test", "."]
```

## The honest exits

An agent may stop with `needs_input` of kind `question` (it needs the
operator) or kind `workflow` (the workflow it was given is wrong for the
task, or a step it needs does not exist). Both end the task as `blocked`
with the text as the reason. Neither is a failure, neither is retried, and
neither is penalized anywhere. Workflow requests are the demand signal for
new workflows; they are counted, not acted on automatically.

## Cost and choice

Every attempt row carries its step and its task's workflow. Per repo and
per workflow that gives a success rate, cost per verified success, and
later a defect rate from human review and a conflict rate at integration.
Choosing a workflow is a comparison of rows with intervals. The
optimization is a table a human reads; later, a policy that picks the
cheapest workflow that clears a bar. It never changes what verification
means: a workflow may add verifiers, never remove one, and only the
kernel's verdict promotes.

## Mechanism, not a CLI

The binary enforces what a valid workflow is and nothing more: loading
refuses a file that fails `check` (name matches file name, kinds from the
closed set, at least one step, positive cost factor, no duplicates, no zero
limits), task creation fails while any file in the directory is broken,
and every task records the hash it ran under. `forge doctor` reports the
directory's state, including uncommitted changes; the directory is a
plain git repository and committing is plain git.

Authoring, editing, and committing workflows are interface, and belong to
the MCP server and skills when the first agent needs them, calling the
same `check` rather than a second definition of validity. The Forge 1
lesson applies: tools are built when an agent needs them, not before.

Not Temporal, not Ansible. The engine is a bit over a thousand lines and
the workflows are a handful of steps on one machine with state in SQLite;
durable-execution infrastructure is ahead of demand until there is more
than one worker machine or a wait measured in days. What is borrowed from
Temporal is the discipline: definitions as data, an event history per run,
idempotent resumption, explicit timeouts.

## Visibility

Every attempt records `inputs` (workflow and its exact text, step, model,
turns, timeout, base and start commits, feedback given, interface given,
overlay refs, whether checks were shown) and `outputs` (end commit, files
changed, dirty files, verify ref, interface produced) next to its verdict
rows and envelope. `forge trace <id>` prints the whole run; `--json` is
the same for tooling. Every terminal failure gets a diagnosis from a table
in `audit.rs`: what happened and what the operator can do. `forge
requests` lists blocked tasks, the demand signal. `forge stats` groups
outcomes by workflow hash and by step.

## Not built

Workflow conditionals or variables. Per-step prompts as configuration. A
step that runs outside the sandbox. A step the kernel cannot verify
afterward. A workflow editor.
