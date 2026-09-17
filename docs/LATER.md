# Later: Forge 1 ideas worth revisiting, and when

Curated 2026-09-11 from Forge 1's TODO, its whole-tree audit, its build
log, and its docs. Ported already: resume past the turn cap, a refused run
does not count, `needs_input.tried`, and the two doctrine tests (every
built-in is documented; every reason has a diagnosis). What follows is
what to revisit, each with the trigger that makes it worth doing.

## When Forge 2 chooses workflows or models itself

- **Model roles as a table, preference as a prior.** Forge 1 grew six
  one-off ways to name a model per job. The fix was one role table
  (decider, planner, reviewer, ...) with per-workflow overrides, and the
  operator's preference modelled as a prior the measurements can
  overrule, never a hard override: an override switches off the measured
  routing. Rejected there and here: a "power level" scale; `class` and
  `local` are different axes (capability versus location).
- **Spend thermostat.** Tie learning spend to calibration once the
  profiles have history: keep spending while well-calibrated and
  improving, taper when flat. Needs many measured workflows first.

## When tasks spawn tasks

- **Provenance.** `caused_by` and a root task id on every task, so a
  decomposition tree is walkable both ways. The root is itself a task;
  no new entity. Comes with `--after` and decomposition.
- **`deps` as a serialized pre-step** was reported and never built in
  Forge 1; `--after` is the Forge 2 shape.

## When a second repository exists

- **Tool library and cross-repo index.** Forge 1 found agents wrote 48
  scripts into projects and never called the shared library once; the
  library had two toys. Operations are the Forge 2 answer at the repo
  level. The cross-repo part (fingerprint every repo's checks and
  scripts, flag a shape present in two or more) only pays with two repos.
- **A platform ask channel** (`forge_request`): a typed, non-blocking
  "this tool or workflow is missing" that must record what the agent did
  instead, searches open requests so duplicates become votes, and turns
  into a task past a threshold. `needs_input` of kind `workflow` is the
  seed; the recording rule is already ported.

## When unattended runs span redeploys

- **Quiesce, don't kill.** Forge 1 paid to keep attempts alive across a
  restart and then killed them as orphans. Forge 2's two-signal stop is
  the quiesce; confirm a rebuild during a night run drains instead of
  requeueing, and that a task resumed by a new worker keeps its session.
- **Resource telemetry as a signal, not a page.** A runner answering
  while degraded must reach doctor and routing before any dashboard.
  Relevant only once a local model runner exists here.

## Rejected, with the reason

- **The constitution as a document.** Rules earn their way in by
  rejecting something. Four of its articles are already mechanism here:
  agents never run in a checkout, Forge computes every number, claims
  need evidence, pushes only through the gate.
- **A network listener.** Forge 1's four critical audit findings were all
  an unauthenticated TCP surface and unsandboxed scratch execution. No
  listener until it has auth; nothing runs outside the sandbox.
- **Mobile app, voice channels, family hosting, comms pipeline, Windows
  port, context-compression proxy, a first-party execution harness.**
  Product decisions for a Forge 2 that has finished being a kernel.

## From Uber's "efficient software factory" (2026-09-13)

Three levers we don't pull yet, each measurable with what exists:

- **Ground the coder.** Their one big number: the same task took 38 s with
  a context graph and 20 min without, and ended wrong. Our capped attempts
  spent their turns exploring. For one repo the graph is `docs/SYSTEM.md`
  (the `graph` directive keeps it) plus git; feed it, and the files the
  task likely touches, into the coder's prompt. Measure turns per attempt
  before and after. See "Context" below.
- **Cost anti-patterns in the audit.** Their session dashboard flags 16
  anti-patterns with a dollar figure. We keep every attempt's full agent
  stream, so "turns before the first edit", "the same file read N times",
  "capped with a dirty tree", "a tests step over N lines" are computable
  rows for `stats`, not opinions.
- **Cheap models on well-specified subtasks, measured.** Their rule:
  frontier model decomposes, cheap model executes the well-defined piece.
  `cheap` (haiku, 15 turns) has never run. The second bench's small chores
  are the test; profiles plus hidden tests are the benchmark.

Not for us at this scale: the MCP gateway, tool projection, code-mode,
and the context graph as infrastructure. The CLI already caches.

## Prompts: the frame in code, the voice in the file (2026-09-13)

Prompts are assembled in the engine from pieces, no templates: the rules
the kernel enforces (untrusted content, commit and leave clean, the result
contract, the honest exits, protected paths, the namespace), then the
contract's role paragraph, the action's `brief`, the task, the interface,
the journal, the feedback. The exact text is the first frame of every
attempt log. Keep the rules in code, since each one has an enforcer and a
test; move the role paragraph into the action file as a `prompt` field, so
a rewording is a new action hash the profiles measure and git can revert,
and a future authoring tool has something to touch. No template language
beyond that: the assembly order should stay in one place tests can read.

## The inspector: a debugger's view of a task (2026-09-13)

Lower priority, noted so it is not lost. A human view of exactly what
happened at each step of a task, stepped through like a debugger: task,
workflow step, attempt, then frame by frame inside the attempt (the exact
prompt, each tool call with its arguments and result, each edit, the
verdict), with the tree and the checks as they stood at that point.

Everything it needs is already recorded, so it is a reader, not a new
recorder: the attempt logs hold the prompt and every CLI frame stamped
`forge_ms`; `forge trace --json` holds the step and attempt structure;
the tool facts hold timings; `events.jsonl` holds the kernel's side. Two
modes fall out of that. Post-mortem: step through a finished attempt,
forward and back, with a diff of the worktree at each edit (`git` can
reconstruct it from the commits and the frame's edit contents). Live: a
breakpoint, `--break-at <step>`, that holds the task before a step the
way a needs_input holds it, so a human can look at the clone and the
context the next directive will receive, then continue or stop.

Belongs in the TUI as a screen, and later the web app, over the same
snapshot and event subscription; the kernel adds only the breakpoint.
Worth picking up once the escalation ladder is in, since the questions
it answers ("why did attempt 3 go wrong here?") are the ones a supervisor
will need to answer too.

## Early-ending thresholds as config (2026-09-14, knob added 2026-09-15)

The watcher that ends an attempt when enough signs of going nowhere trip
(`Watch` in src/agent.rs: thirty calls without an edit, fifteen edits
without a commit, one command run five times, any two together) had its
thresholds and its "any two" count as constants. They are guesses until
the 100-turn regime has produced data; a replay over the first 244
attempts found no case they would have stopped, because the old 30-turn
cap ended everything first.

The knob now exists: `[early_ending]` in the operator's data-dir config
(`no_edit_calls`, `edits_without_commit`, `repeats`, `signals_to_end`,
defaults matching the numbers above; `signals_to_end = 0` disables early
ending). Every attempt now also records, from `Watch`, which signals
tripped (`early_signals`) and which were within 20% of tripping and did
not (`early_near`), both as JSON columns on `attempts`, so the values can
be tuned from the store once they have earned themselves. Still missing:
a per-task override on `forge add` shaped like `--max-turns`, and the
actual tuning pass once `early_signals`/`early_near` have accumulated
enough rows to read. The read-only exemption for the review and plan
contracts stays in code: it is a property of the contract, not a
preference.

## A second runner for the supervisor (2026-09-14)

The supervisor's contract is runner-agnostic: a prompt in, a structured
ruling out, nothing written, verified by the kernel. Today every agent
runs through the claude CLI. Putting the supervisor on another
provider's strongest model means a second backend behind `agent::run`
that produces the same stream (tool calls, a result with structured
output and cost) or a thin adapter that does. Worth doing once the
supervisor's decisions have an outcome record to compare models on.

## A `history` operation and per-step context budgets (2026-09-13)

Moved here from docs/CONTEXT.md, which now carries only the status of
what shipped. `repo-map` and the journal (`forge journal`) covered the
two sources below; `history` as its own operation and the explicit
budget were never built.

One mechanism, three sources, one measurement.

- **Mechanism.** Operations may `produces = ["context"]`. What such an
  operation prints, cut to a per-step budget (default 1,500 tokens),
  is appended to the next directive's prompt under a heading, and
  recorded verbatim in the attempt's `inputs` so the audit shows exactly
  what the coder was told. A kernel operation `history` does the same
  from the store.
- **Sources, in the order to build them.** `history` first, because
  it is the strongest effect in the evidence and unique to us:
  every landed task's summary, changed files, and hidden-test
  interface, plus every prior attempt in this lineage and why it
  ended — "the last attempt failed L1 test on planetesimals pricing;
  task 33 landed light upgrades touching data.ts and tick.ts" is the
  200-token summary the literature found most effective, and it is a
  query, not a model call. `repo-map` second, ranked by task words,
  ctags-based. The repo's `CLAUDE.md` third, hand-written from the
  rules already in the preamble. A static map (`docs/SYSTEM.md`) rides
  along for free once `repo-map` exists; seeding whole files not at all.
- **Measurement.** Two workflow versions differ only by the context
  operation; tasks alternate between them; the profiles compare turns
  before the first edit, tool calls, attempts, and cost per piece of
  work. `stats` gains "turns before first edit" as a column so the
  effect is read from data, not felt. If a source does not move the
  number, it goes.

Worth revisiting once `forge journal`'s always-on text form needs the
same per-step budget and heading treatment that a `produces = ["context"]`
operation gets, or once a source is added that isn't already a kernel
command in its own right.

## A retry should start from its parent's branch when that branch was verified (2026-09-14)

Task 155 verified, was demoted by review, the supervisor answered, and
the retry (162) started from a fresh clone of main: everything 155 had
built was thrown away and rebuilt, then 162 died and its branch had to
be landed by hand. When the parent's last code attempt passed the
checks, the retry should clone the parent's branch and be told what the
review found, so the second agent finishes rather than restarts. The
fresh clone stays right when the parent's checks failed.

Also: the audit's diagnosis has no arm for "stopped early", so the
first live early ending (162 attempt 1: fifteen edits without a commit,
one grep run five times, resumed and then productive) was described as
"the agent process failed outside Forge's rules".

## What a capped attempt costs at 100 turns (measured 2026-09-14)

The turn guard went from 30 to 100 on the argument that cost and wall
time, not turns, should bound the work. The first measurements of that
regime, from the plugin chain:

- Two attempts have reached the 100-turn guard. They cost $4.18 on
  average, against roughly $0.50 for a capped attempt under the old
  30-turn cap.
- Neither was a runaway. Task 194's first attempt made a commit across
  ten files, read one file four times, and explored 25 calls before its
  first edit: all under the early-ending thresholds, correctly, because
  it was working rather than spinning. The resume rule then let its
  second attempt finish the same session in 20 turns.

So the lesson is not that the early-ending thresholds are too loose.
A long, productive attempt is meant to cost what it costs, and the
per-task budget is the guard that worked (194 landed at $7.92 against
an $8 cap). The lever on cost is task size: 194 asked for a catalog, a
store migration, a view row, two CLI verbs, docs and tests in one
piece of work, and cost five times the $1.48 mean. Ask for less per
task before reaching for the threshold knob.

Still worth doing when there is more data: make the thresholds and the
"any two" count configurable per the note above, and record on the
attempt which signals were near tripping, so this question can be
answered from the store instead of by reading logs.

## Defect escape: the measurement we do not have (2026-09-15)

We measure cost per landed piece of work, attempts, turns, and calls
before the first edit. We do not measure whether landed work was any
good. The independent research on agentic development (DORA, Faros,
GitClear) is consistent that throughput without a quality counterweight
is the number that misleads: teams ship more and merge more while
review time and change-failure rates rise.

The metric to add is defect escape: of the tasks that landed, what
share did a later task have to repair. Two signals are already in the
store and need no new recording. A task that repairs an earlier one is
the direct case, and the retry chain and the decisions table already
link them. A check that fails on a base a previous task landed is the
indirect case, visible in the first attempt of the next task on that
repository: its L1 rows fail before the agent has changed anything,
which today reads as "the repo was broken" and is attributed to nobody.

Worth doing as a column or a `--quality` flag on `forge stats`
reporting, per workflow, the share of landed tasks followed by a repair
and the share of tasks whose first attempt failed a check on an
untouched base. Both are queries, not new instrumentation. Until they
exist, a clean landing rate is a claim about speed that says nothing
about correctness.

## Digital twins of external dependencies (2026-09-15)

Taken from StrongDM by way of the 2026 software-factory survey
(docs/research/05-software-factory-2026.md), and recorded here because
it is a verification technique we lack rather than an idea we invented.

An agent can only iterate against something it can run. When the thing
under test talks to an external service, the checks either hit the real
service (slow, rate limited, unable to fail on demand, and requiring
credentials inside the sandbox) or they mock it by hand, which drifts
and ends up testing the mock. The third option is to generate a
self-contained behavioural clone of the service from its public API
documentation and reference SDKs, run it locally, and point both the
checks and the agent at the clone. Thousands of runs an hour, no keys,
and failure modes you can request.

This matters in two places. It is the verification moat for the
operated model in docs/GTM.md, where a customer's automation is mostly
integration and a shallow check is worse than none. And it is the shape
of a hidden suite for any task whose correctness depends on something
the sandbox cannot reach, which is why the egress policy and this
technique are the same conversation: a twin is how you say no to the
network without saying no to the test.

## Autonomy has two axes, not one (2026-09-15)

The industry's ladder (spicy autocomplete, intern, pair, reviewer,
engineering team, dark factory) measures one thing: what merges without
a human. By that measure Forge is at the top rung, since the kernel
lands on main and no person reads the diff.

That is a poor description of what Forge does, and the mismatch is
worth holding on to when reading anyone's autonomy claim. Code autonomy
and intent autonomy are separate axes. Forge is fully out of the loop
on code: the checks decide and no person approves a change. It is
deliberately in the loop on intent: the investigate directive stops and
asks when a task is impossible or contradictory, a reviewer may demote,
and the supervisor escalates whatever the record cannot settle. The
escalation ladder exists to keep the second axis honest while the first
runs unattended.

A system can sit at the top of one axis and the middle of the other,
and which one a vendor means is usually the difference between an
impressive claim and a true one.

## The journal measurement was ill-posed three times (2026-09-15)

Three batches tried to measure whether showing an agent the journal (what
earlier attempts in this piece of work said, and what the checks found)
helps. All three failed, and not because of noise. The design was wrong,
for a reason worth recording so nobody attempts a fourth.

**The journal is empty on a fresh task's first attempt.** It lists the
attempts already made in this lineage, so the first agent to touch a new
task is handed nothing whichever arm it is in. Of 89 recorded first
attempts on the code contract, only 16 carried a journal, and every one
of those was a retry task inheriting its parent's history. Pairing two
fresh tasks and turning the journal off on one therefore compares two
identical prompts, which is what the last pair did: both landed in one
attempt, neither saw a journal, and they came out at 45 turns and $1.06
against 25 turns and $0.58.

That number is the useful thing salvaged from the exercise. **The same
prompt, the same base, the same model, twice, cost about twice as much
one time as the other.** Any A/B on single tasks has to clear that noise
floor, which means many pairs, not one.

**The measurement that can be made** is retrospective, over attempts that
could actually have had a journal: retries. Across all history, among
code attempts after the first:

| arm | attempts | mean turns | first edit at | succeeded | mean cost |
|---|---|---|---|---|---|
| journal | 47 | 30.0 | 9.6 | 55% | $0.60 |
| no journal | 8 | 28.4 | 5.1 | 38% | $0.62 |

Eight control attempts is not an answer. It leans the journal's way on
the outcome that matters (whether the retry succeeded) and against it on
exploration, and neither lean survives contact with the sample size.

**What to do.** Stop putting `--no-journal` on fresh tasks: it changes
nothing and buys a confounded comparison. If the question is worth
settling later, assign the control arm to a fixed fraction of *all*
tasks and let the control group accumulate on retries by itself, then
read the table above when the control column reaches a few dozen. Until
then, keep the journal: it is free on the attempts where it is empty,
and costs nothing measurable on the attempts where it is not.

## The rate-limit experiment (2026-09-15): what happened and what did not

The question was how Forge behaves when the subscription runs out, and
how it resumes. The weekly cap was raised from 95% to 100% at 10:20 with
the window at 93%, and about $70 of real work was queued so the provider
would refuse an attempt before the 14:00 reset.

**What did not happen.** The provider never refused. The window read 99%
from 12:57 to the reset, and the CLI reports utilization as a whole
percent, so the last observable value before 100 is 99 and there is no
way to see how close an attempt is. Thirty-one attempts ran in that last
percent. The refusal path in `agent.rs` (a `rate_limit_event` with
status `rejected`, or an error result mentioning a rate limit), the
refund in `engine.rs`, and the worker's sleep until the reset remain
exercised only by fakes. Next week, run the same experiment from 97%
with more fuel queued, and accept that hitting the edge is a matter of
luck at whole-percent resolution.

**What did happen.** The pacing half worked: with the cap at 100% the
worker ran to the last percent without holding, which is the
use-it-or-lose-it policy the economist note describes, done by hand.
Every sample carried `status: allowed_warning` and
`surpassedThreshold: 0.75` from 93% on, so the CLI does signal the
approach, just not finely.

**What the day surfaced instead**, none of it about the window:

- A verified code attempt followed by budget exhaustion before review
  ended as failed with the work stranded on its branch (230, 235, 239).
  Fixed the same day: it now ends unverified and `forge land` accepts it.
- The escalation ladder had no "never mind": a task escalated to the
  operator whose answer is no had only answer and retry. `forge withdraw`
  exists now and was used on the task that needed it.
- The review step's 40-turn cap is too tight for kernel-sized diffs:
  eleven review attempts in one day ended by running out of turns or
  failing to produce structured output. The reviewer only reads; give it
  room.
- The integrator's check run leaves no record, so an integrate-time
  failure the coder cannot reproduce (232) is undiagnosable. Task 239
  records it as an attempt row.
- An attempt that exhausts its attempts on `has-commits` after an
  integrate rewind fails with an empty reason.
- An intermittent bubblewrap failure binding `~/.claude.json` inside a
  nested sandbox took out 98 e2e tests in two different trees. A
  200-run probe of the same bind passed. Cause unknown; the integrator
  record will catch the next one.
- Two tasks (225, 230) hit the 100-turn guard with a dirty tree on
  their first attempt: task size again, not the guard.

The cap is back at 95% and the config matches the copy taken before the
experiment.

## The code visualiser, and the inspector inside it (2026-09-15)

Three layers, in order, each on something that exists.

**Structure, deterministic.** Modules, files and symbols as nodes;
imports and calls as edges. `forge-repomap` already extracts symbols per
file with a content-addressed cache; adding import edges (Rust `use`,
TypeScript `import`, Go imports, Python imports) is an extension of the
same extractor table, and the output is a graph file regenerated on
every landing in milliseconds. This replaces the model-written system
map for everything an extractor can see; the model's remaining job is
annotating data flows on the deterministic graph. No model draws the
graph.

**The overlay, from the record.** Every attempt records the files it
changed and its cost, so each node carries: tasks that touched it and
when, cost sunk into it, review demotions on changes there, and defect
escape by path once delayed cost is attributed to paths. The live part
is the running attempt's tool calls, which the early-ending watch
already sees: where the agent is right now. This is the layer nobody
else has: where the factory's money and mistakes go, per module.

**The inspector.** A task's run as a scrubbable timeline: steps,
attempts, every tool call from the log, the graph highlighting what
each call read or changed. Replay for finished tasks, tail for running
ones; all already recorded. A real debugger follows: the claude CLI's
tool-call hooks can block, so a pre-tool hook that waits for Forge's
say-so gives breakpoints on a tool name or a path and single-stepping,
in the runner, with no model involved. (This folds in the earlier
"inspector" note.)

Lives in the web client as a graph page (SVG layout, the existing event
stream). Kernel changes: the extractor edges; later the pause hook.
Sizing: extractor plus page, three or four tasks; overlay, two more
after delayed cost; the stepping debugger, its own initiative.

## The local model, measured (2026-09-17)

Job step 2b (docs/JOBS.md, "Directive steps") ships the first bounded
judgment small enough to run the same way on every candidate provider:
`changelog-line`, a run workflow with one directive step (`role
= "summarise"`, schema `{line, kind}`) that turns a landed task's text
and diff stat into a one-line changelog entry, then an operation that
appends it to `CHANGELOG.md` in the job's scratch. It lives where
docs/JOBS.md says an automation lives — `.forge/workflows/changelog-line.toml`
and `.forge/fixtures/changelog-line/*.json` — four fixtures built from
real landed tasks in this repository (e25c5e2, 71603be, 37b9011,
79fec7f), one per `kind`.

`forge job bench <project> <workflow> --providers a,b` is the harness:
every fixture, once per named provider, in dry-run mode, with the
directive step's role forced to that provider regardless of the
operator's or project's own routing, so it is the same judgment on the
same input under each candidate rather than whatever routing happened
to be configured. It prints schema-valid share, expected-kind share,
mean cost and mean seconds, and (because it runs the real executor) it
leaves an ordinary row per run in `jobs`, all dry.

The numbers below came from the e2e suite's FAKE providers standing in
for anthropic and devhome, not from the models; the task was asked for
the real run and recorded the fakes. The real run on 2026-09-17 scored
0/4 schema-valid on both providers (anthropic produced structured
output the bench did not accept; devhome failed to launch in 0.01 s),
which is a harness defect under diagnosis, not a measurement.

What the fakes produced, kept only to show the table's shape:

```
provider     runs  schema-valid     expected-kind     mean-cost mean-seconds
anthropic       4  4/4 (100%)       4/4 (100%)          $0.0021        0.01s
devhome         4  4/4 (100%)       3/4 (75%)           $0.0000        0.21s
```

Both providers stay inside the schema every time — the shape of the
judgment is not what is hard here. The gap is in the judgment itself:
the stand-in for the local model missed the one fixture built from a
bug fix that reads, out of context, like routine cleanup (e25c5e2, a
five-file store change with no user-facing symptom in its own commit
message). That is exactly the kind of miss `bench` exists to catch
before a local model is trusted with a real judgment step, and exactly
why the comparison has to be the same fixtures, the same schema, the
same run — a hosted-versus-local number from two different prompts or
two different days is not a comparison. No real local runner is wired
in yet (see "When unattended runs span redeploys" above); once one is,
the next run replaces the fake provider with it and the table here is
the baseline to beat.
