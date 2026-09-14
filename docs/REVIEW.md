# Architectural review and refactor plan (2026-09-14)

Forge 2 is six days old, 21,000 lines across the kernel, three clients,
92 e2e tests and 56 fake agents, and it has landed its own last thirty
changes. Every rule in it was added where a real run needed it. This
review reads the tree cold, five slices in parallel by readers with no
memory of how the code got here, then confirms each concrete claim at
its line. The findings are grouped by the cost they share, and the plan
is staged so that every stage lands with the suite green, through Forge
itself wherever the work is precise enough to hand it.

Sizes at the time of reading: engine.rs 2583 lines with `run_task` at
850; cli.rs 2166; workflows.rs 1607; verify.rs 1427; store.rs 1380;
tests/e2e.rs 4458 in one file. Commits per day since the ninth: 15, 19,
10, 8, 66, 19.

## 1. Defects confirmed while reading

Fix these first; none needs a refactor.

1. **A failed final push leaves the task `succeeded`.** engine.rs:833
   sets `last = Succeeded` before the push; on `Err` at 858 only
   `last_reason` changes, so the task ends succeeded with reason "push
   failed". State and reason are derived separately (see theme 2).
2. **Every kernel `verify` row in the trace records 0 ms.**
   engine.rs:503-516 passes `Instant::now()` as the row's start.
3. **`repo-map.toml` is in `BUILTIN_OPERATIONS` twice**, byte-identical
   (workflows.rs:418, 430). `ensure` writes the first and skips the
   second; no test parses built-ins individually.
4. **A supervisor that times out or crashes is recorded as
   `checks_failed`, never `agent_failed`** (supervisor.rs:337 folds
   `agent_failure` into the `result-structured` row), so the audit's
   agent-failure diagnosis cannot see it.
5. **`update_task` writes 16 of the Task's 37 columns** (store.rs:635-
   660). `land`, `after`, `budget_usd`, `journal`, `retry_of` and others
   are insert-only by accident of history; a field mutated after insert
   silently does not persist. Latent today, one bug away from real.
6. **`docs/ACTIONS.md`'s built-in table is wrong for six of eleven
   workflows** (it predates `repo-map` and `fmt`), and the doctrine test
   at workflows.rs:1586 only checks that names are mentioned. Agents
   read that table as instructions.
7. **README's "Deliberately absent" list (191-197) is entirely present**
   (web UI, answering questions, the learning loop), the layout omits
   five modules and three crates, and it says the web client is
   read-only; `docs/BACKLOG.md` is ten items all done; `WORKFLOWS.md`
   carries an older step table and calls the engine "two hundred lines".
8. **`dirty_paths` and `workflows::uncommitted` parse porcelain two
   ways** on differently-trimmed output (git.rs:412, workflows.rs:1169);
   the first bit the supervisor yesterday.

## 2. Themes

### 2.1 Closed vocabularies kept as strings

Contract names are compared as literals at eight sites in engine.rs and
validated as a `&[&str]` in workflows.rs; products the same; the L0 rule
names exist only as literals at nineteen sites and are re-matched by
string in `verify_review` (blacklist), `verify_plan` (whitelist),
`audit::diagnose`, and the tests; `needs_input.kind` lives in four
places; event types in three; and "landed" is a free-text `reason`
prefix that the claim query (`store.rs:684, 713`) and three Rust sites
match with `LIKE 'landed %'`. The cost is uniform: a new common rule
lands in one contract and not another, `diagnose` has no arm for ten
rule names and no test can tell, and rewording the landing message would
stop dependents being claimed.

### 2.2 The run is an implicit state machine

`run_task` keeps fourteen mutable locals (`used`, `owed`, `idx`, `seq`,
`last`, `last_reason`, `all_ok`, `budget_stop`, `review_unfinished`,
`stalled`, `landed`, `capped_committed`, ...), has three hand-rolled
rewinds (verifying op, tests fault, landing conflict) that bookkeep
differently and gate on budget differently (attempts only at 337 and
541, attempts and cost at 789), derives the terminal state by prefix
inspection of `last_reason` (831, 924), and pushes on a four-way
disjunction of flags added one outcome at a time. Defect 1 is what that
costs; every new outcome is a flag and an arm.

### 2.3 Copied skeletons

The same shape written several times, so a change is made several times
or missed:

- Four contract verdicts (`verify`, `verify_tests`, `verify_review`,
  `verify_plan`) plus the supervisor's own build the same
  agent-failure, common-L0, contract-rows, emit, decide, override
  sequence, from seven hand-built `Verdict` literals that start in a
  state the engine treats as unreachable. `common_l0` returns a
  seven-tuple that four callers destructure positionally.
- Four `run_*_attempt` runners share journal, prompt, `Inputs`,
  `new_attempt`, `launch`, verify, `record`, and differ in five data
  points. `Inputs` is assembled four times and overwritten again in
  `new_attempt`. The review runner takes no feedback, so a verifying
  operation after a review drops what it owed.
- Three prompt builders re-append the same context, journal, attempt
  line and step suffix in slightly different orders; the paths and
  namespace rules are prose here and code in verify.rs with nothing
  tying them.
- The journal's said/found derivation is copied between `journal_for`
  and `journal_entries_for`, including the rule that a test author's
  words never reach the coder.
- Every CLI verb has a text branch and a JSON branch written by hand;
  `show` is a third rendering of the trace with no JSON; `stats --json`
  uses column headers as keys (`"$/OK"`, `"EDIT@"`).
- `Event` is matched three times in report.rs (summary, JSON, terminal).
- Eight ways to spawn git: the shared runner plus seven functions with
  their own spawn and error text; the identity is hard-coded three times.
- The current/previous/regressed profile is computed in cli.rs and again
  in doctor.rs; the worker-pid probe twice.

### 2.4 Kernel logic in the CLI

`enqueue_with`, `map_dep`, `retry_args`, `answer`, `integrate` and `land`
live in cli.rs on the clap `TaskArgs` struct. `retry_args` round-trips
`Task -> TaskArgs -> Task`. The supervisor, a kernel module, reaches
into cli.rs twelve times to queue work, and duplicates `answer`'s
six-step sequence with different text. No event is emitted when a task
is queued, retried, answered or blocked by a dependency, so the README's
"clients re-read only when an event says it changed" is not true for
queue changes.

### 2.5 Hand-kept plumbing

`Task` is 37 fields mapped by positional `r.get(N)` from a
hand-maintained column string; `Attempt` is 36 with a 28-parameter
`finish_attempt`; nothing checks the lists agree, and adding a column is
five edits in three places (defect 5 is the consequence). `op()` takes
twelve positional arguments at fifteen sites. Built-in actions and
workflows are 430 lines of TOML as escaped Rust strings, validated only
because a workflow happens to reference them.

### 2.6 The client contract is undefined

The JSON the TUI and the web page depend on is a set of `json!` literals
in cli.rs. Field names drift across verbs: task text is `task` in the
listing and `text` in the trace; time is a localtime string in one and
a unix `ts` in events; requests put the task under `task` and the
request under `text`. Both clients hand-parse with silent defaults, and
each keeps its own list of which event types invalidate what (seven in
the TUI, four, two and three in the page's three views). The boundary
test forbids linking the kernel but nothing pins the schema, so a
renamed field blanks both clients without a test failing.

### 2.7 Tests

One 4458-line file in no topic order. About forty assertions couple
tests to exact log wording that is also available from `trace --json`.
Seven timing assertions and two fixed sleeps make the suite depend on
load and cost around thirty seconds. The trace-parsing incantation is
retyped 25 times; the per-role fake setup ten times. Fifty-six fakes
each hand-write the same 300-character envelope, with near-duplicate
pairs differing only by a sleep. Untested: `forge land`,
`forge supervise`, `--poll`, `events --follow`, SIGTERM drain, four of
the operation env variables, the fifteen-edits sign, three workflows,
and bubblewrap itself, since the harness silently disables the sandbox
when bwrap is absent, which makes the whole sandbox column vacuous on
such a machine.

### 2.8 Sandbox, git, repomap, small things

`Sandbox::detect` reaches back into `ctx` and `agent` (a module cycle)
and creates a directory as a side effect; its argv ordering invariant is
a comment with no test. `git()` trims output, which `dirty_paths` undoes
heuristically. Fault classification for git has no stated rule: a dead
remote at clone time fails the task rather than stopping the worker.
`repomap`'s extractor is one 135-line loop with a quadratic dedup and no
test for Go or Rust traits, and its argv loop panics on a trailing flag.
`load_home` writes a file as a side effect of reading, so every
read-only verb mutates the data dir; home defaults exist twice.

## 3. Keep

The readers agreed on what not to touch: `decide` as a pure table-tested
function; `run_one_capped` with its process-group kill and drain grace;
the claim rule and `tests_fault`; `claim_next` as one atomic `UPDATE ...
RETURNING`; the forward-only numbered migrations with the refuse-newer
guard; `profile.rs`; `Fault`/`Classify` with `worker::drive` as the one
conversion site; the `Watch` early-ending in agent.rs; `record` as the
single writer of an attempt row; `check_flow` and `splice`; the fake
protocol and per-role `FORGE2_CLAUDE_BIN_<ROLE>` seam; the two doctrine
tests; the event log shape with `snapshot` returning an offset; the web
token flow. Every move below extends one of these rather than replacing
it.

## 4. The plan

Ordered by what each stage unlocks and by risk. A stage lands as one or
more commits, each green. "Forge" marks work precise enough to hand to
Forge as tasks; "hand" marks kernel semantics that need a human at the
keyboard.

**Stage 0. The defects (hand, today). Done 2026-09-14.** Items 1-5 and 8 of section 1 as
small commits with a test each: push failure ends the task failed with
the branch pushed; the verify row takes the attempt's timer; the
duplicate built-in goes and a test parses every built-in on its own; the
supervisor's agent failure is `agent_failed`; `update_task` writes every
mutable column and a test asserts the column lists agree with
`PRAGMA table_info`; one porcelain parser. Docs items 6 and 7 are stage 9.

**Stage 1. Test support (Forge). Done 2026-09-14: 102 e2e tests in eleven area modules, a support module, a fakes library, timing waits on the store, and the sandbox failing loudly when bwrap is missing. Two of seven tasks landed by hand after review demotions; the rest through Forge.** `tests/support/mod.rs` with `Env`,
`git`, `check`, `trace_json`, `requests_json`, `decisions_json`,
`with_role`, `wait_until`; the e2e file split by area under one
`tests/e2e/main.rs` so it stays one binary; sleeps and minimum-elapsed
assertions replaced by `wait_until` on the store and by the recorded
hold; a `tests/fakes/lib.sh` that emits the envelope so fakes become
two-to-five-line callers, keeping `ok.sh` hand-written as the visible
schema; the missing tests for `land`, `supervise`, SIGTERM drain, the
operation env, the fifteen-edits sign; a sandbox test that fails loudly
when bwrap is missing. This comes first because every later stage needs
a suite that is fast, grouped, and not coupled to log wording.

**Stage 2. Typed vocabularies (hand). Done 2026-09-14: Contract, Product and Kind enums; the Rule registry with an exhaustive audit table; landed_sha.** `Contract` and `Product` enums in
workflows.rs with `serde(rename_all = "lowercase")` so the stored
`resolved` JSON is byte-identical (a round-trip test proves it);
`Kind` in envelope.rs; a `Rule` enum whose `name()` returns today's
strings, used by every `l0(...)` site, with `verify_review` and
`verify_plan` selecting by variant and a test that every `Rule` has a
`diagnose` arm; a `landed_sha` column (migration, backfilled from the
reason prefix) read by `claim_next`, `block_dependents`,
`workflow_stats`, `map_dep` and `land`. Engine string compares become
enum matches and the `other =>` arm disappears.

**Stage 3. Store plumbing (Forge, after 2). Done 2026-09-14: named columns, a FinishAttempt struct, one lineage query, tool statistics in one query; four tasks, one review catch, all landed by Forge.** Named columns via
`r.get("name")`, one `TASK_COLUMNS` list the SQL is built from, the same
for `Attempt`; a `FinishAttempt` struct for the 28 parameters; one
lineage query used by `root_of`, `runs`, `decisions_in_lineage` and
`supervisor_answers_in_lineage`; `collect_tool_stats` as one query.

**Stage 4. Kernel seams out of engine.rs and cli.rs (hand). Landed 2026-09-14: journal.rs, prompts.rs, queue.rs (with a task_queued event), landing.rs, operation.rs; engine.rs 2583 → 1540 lines. Still open from this stage: OpRow and a Timer for the op recorder's twelve arguments; Rewind returning the new base instead of mutating the task.** In order:
`journal.rs` (entries derived once, `journal_for` renders them);
`prompts.rs` with a `PromptCtx` and one shared tail, landed
byte-identical first against the `forge_prompt` frame every log carries,
then deduplicated; `queue.rs` with a plain `TaskRequest`, holding
`enqueue`, `retry`, `answer(by, citations)` and emitting
`task_queued`/`task_blocked`, with `From<TaskArgs>` staying in cli.rs and
the supervisor calling the kernel; `landing.rs` with `integrate`
returning the new base in `Rewind` rather than mutating the task;
`operation.rs` with an `OpRow` and a `Timer` replacing `op()`'s twelve
arguments. `run_task` comes out at roughly 400 lines.

**Stage 5. One verdict path (hand). Done 2026-09-14: one Subject for every contract, GitFacts and Common in place of the tuple, Verdict::open/settle in place of seven literals, the contract in the decision table, one verify_directive with the contract's rows under a match, emit_check shared, the supervisor's verdict built the same way, From<Fault>, a lenient Ruling.** `Verdict::new(facts)` and
`settle(...)`; `GitFacts` and `Common` structs replacing the tuple;
`emit_check` used everywhere; `Contract::common_rows()` replacing the
name lists; one `verify_directive(contract, subject, agent)` with a
per-contract `extra_rows` hook; the `Unverified -> Succeeded` rule moved
into `decide` with the contract as an argument so the table test covers
it; the supervisor's verdict moved into verify.rs; `From<Fault> for
anyhow::Error`; `#[serde(default)]` on `Ruling`.

**Stage 6. The attempt runner and the explicit run (hand, last in the
kernel). Done 2026-09-14: attempt.rs with one run_attempt over a per-contract Spec; the run is a Run cursor with one rewind and an End value set once at the point that decides it, from which the push decision and the task state derive. engine.rs is at 1090 lines from 2583; every fake's terminal path passed unchanged.** `attempt.rs` with one `run_attempt` driven by an
`AttemptSpec { dir, writes, verify, verify_ref, extra_inputs }`, with the
review-feedback gap decided explicitly; then `struct Run` with one
`rewind(to, kind)` that does every bookkeeping step and returns the side
effects, and `enum End { Landed, Verified, Unverified, Blocked, Failed,
Budget }` from which the push decision and the task state are derived.
The fakes `needsinput`, `cappedcommit`, `cappedresult`, `crash`, `hang`,
`flaky`, `reviewer-demote`, `reviewer-lazy`, `planner-lost` each pin one
terminal path; run before and after.

**Stage 7. Views and events (Forge, after 3). Done 2026-09-14: six tasks, six first-pass landings.** `view.rs` with
serializable `TaskRow`, `RequestRow`, `DecisionRow`, `TraceDoc`,
`StatsDoc` built once from store types; text renderers take the struct;
one set of names (`text`, `created_at` as unix, `cost_usd`, `kind` parsed
by one function) with the old keys kept as aliases for one release;
`show` becomes a renderer of the trace; `Event` serialized directly with
`#[serde(tag = "type")]` and a `summary()`; `doctor` as a list of check
functions reporting the real `user_version`; the profile measure shared
with cli. `stats --json` keys named for what they are.

**Stage 8. The client contract (Forge, after 7).** `docs/CLIENT.md`
listing the verbs, every field each client reads, and which event types
invalidate which list; a `client/` crate (serde_json only) with
`Forge{bin}`, `snapshot()`, `subscribe()` and `#[serde(default)]`
structs, used by the TUI and the web server; a captured
`tests/fixtures/trace.json` parsed by those types and asserted by the
e2e; `index.html` split into page and `app.js` with one `call()` and one
invalidation set per view; a boundary test that parses each manifest
with `toml` and checks every dependencies table.

**Stage 9. Workflows, git, sandbox, repomap, docs (Forge).** Built-ins as
files under `src/builtins/` included by `include_str!`, a per-file parse
test, and the ACTIONS.md table generated and asserted step-by-step by
the doctrine test; one `Catalog` loader with `resolve`/`check` over it
and blob hashes computed once; a `Git` runner with raw and trimmed
output, uniform error text, one identity, one porcelain parser, one
untar; `Sandbox::detect` taking its inputs with an argv-order test;
repomap extractors as a per-language table with `HashSet` dedup and a
bounds-checked argv; a stated git fault rule (worktree operations are
task faults, clone/fetch/lock are environment faults); `load_home` no
longer writing on read and a test that the template equals the
defaults. Docs: README synopsis generated from the clap doc comments and
the "deliberately absent" list deleted; `BACKLOG.md` deleted; the old
table and merge-queue section removed from `WORKFLOWS.md`; `CONTEXT.md`'s
design text moved to LATER with a status section left; `SYSTEM.md`
regenerated by the graph directive once stages 4-6 settle the modules.

## 5. What this buys

Stages 0-3 remove the confirmed defects and the plumbing that made them
possible, and give the refactor a suite it can trust. Stages 4-6 make
the kernel readable by someone who was not here this week: one place
per responsibility, an explicit run, one verdict path, and the
supervisor calling the kernel instead of the CLI. Stages 7-9 give the
clients a contract and turn the docs from claims into generated facts.
Nothing in the plan changes what Forge does; every stage is checked by
the same 92 tests plus the ones stage 1 adds, and the last thirty
landings say Forge can carry the mechanical stages itself.
