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
