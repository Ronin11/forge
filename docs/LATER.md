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

## Early-ending thresholds as config (2026-09-14)

The watcher that ends an attempt when two signs of going nowhere trip
(`Watch` in src/agent.rs: thirty calls without an edit, fifteen edits
without a commit, one command run five times, any two together) has its
thresholds and its "any two" count as constants. They are guesses until
the 100-turn regime has produced data; a replay over the first 244
attempts found no case they would have stopped, because the old 30-turn
cap ended everything first. Once the values have earned themselves, lift
them into a `[watch]` section of the data-dir config, with a per-task
override on `forge add` shaped like `--max-turns`. The read-only
exemption for the review and plan contracts stays in code: it is a
property of the contract, not a preference.

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
