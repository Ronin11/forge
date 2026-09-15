# Actions

Decided 2026-09-10. A workflow is an ordered list of actions. An action is
a directive (an LLM step) or an operation (a deterministic step). The line
between them is the verification boundary: a directive's output is a
claim, an operation's output is a fact, and nothing a directive produces
reaches main without passing through an operation.

## Files and identity

One file per action in `<FORGE2_HOME>/workflows/actions/<name>.toml`, next
to `<FORGE2_HOME>/workflows/<name>.toml` for workflows. The directory is a
plain git repository. The identity of a version is the git blob hash of
the file, the same identity git already gives every version of every
file, readable from history without checking anything out.

Latest by default, no pins, no lockfile. What makes that safe:

- **Resolve once at task start, run from the record.** Creation only
  checks that resolution is possible and that the whole directory is
  sound. When a task starts it resolves the latest versions, records
  every workflow and action hash as a flat list of pins together with each
  file's text, and runs from that record; a resumed task keeps it. An edit
  landing mid-run cannot change a running task, and a task queued Monday
  and started Wednesday runs Wednesday's files, which is what "latest by
  default" means.
- **Revert is git.** `forge stats` shows which hash had the good numbers,
  `forge workflows` shows the commit that introduced each hash, and
  reverting is checking that file out of that commit and committing.
  Every parent that references the action picks it up at its next task.
  Old tasks keep the version they ran.

Pins can be added the day a real need shows up. Nothing here prevents it.

## Directives

A directive is a prompt template, a capability set, and a contract.

- Capabilities: what the agent can see and touch. Whether the verification
  namespace is hidden, whether the acceptance checks are visible, which
  paths it may write, the model, turns, and timeout.
- Contract: the envelope it must produce, and the verification rules
  specific to its output. `tests` adds namespace-only and red-on-base;
  `code` adds namespace-untouched.
- The prompt is the smallest part.

A directive file names its `contract`, the kernel-enforced behavior that
runs it (default: its own name). Four contracts exist: `code`, `tests`,
`review`, and `plan`. Many directives over few contracts: `docs`, `fix`,
`polish`, `document` (docs and comments brought in line with the diff),
and `graph` (the system map in docs/SYSTEM.md) are all the `code`
contract with different parameters. A file naming any other contract is
rejected.

Parameters a directive file may carry: `model`, `max_turns`,
`timeout_secs`; for the `code` contract, `paths`, a write scope that
becomes the L0 row `paths-in-scope`; and `brief`, a short instruction
appended to the task. `brief` is the one prose surface, allowed because
it is versioned, hashed, recorded on every task, and still verified by
the kernel.

A directive may also carry `prompt`, an optional string appended
verbatim to the role prompt the agent receives, as a final section
headed `This step:` — after the task, the interface, and any feedback,
so it is the last thing the agent reads. Unlike `brief`, which is woven
into the task text itself, `prompt` stands apart, for an instruction
specific to how this step should be carried out rather than to what the
task asks. It carries no extra identity of its own: the action hash
already covers the whole file. An operation carrying `prompt` is
rejected; the field is a directive's.

The `review` contract is the executing, demote-only verifier from the
research: a fresh session in the coder's clone that may not write
(`no-writes`), must run something (`executed-something`), and may end the
task as `blocked` for human review with the defect and the command that
shows it. A demotion from a session that ran no tool is recorded as a
note and does not stand. The branch is still pushed so the human can
look. What the human decides is how reviewer precision gets measured. A user-authored
directive arrives when a workflow request shows the need, with one rule:
it names the operations that verify its output. The Forge 1 directives
library was prose agents could also bypass; a directive here is bound to
a kernel-enforced contract, which is the difference.

The `plan` contract is the front door: an investigator that reads the
repository and returns either a plan or a question, before any code.
It may not change the branch (`untouched`), and its plan must be
substantive (`plan-substantive`, at least 120 characters) and name only
paths that exist in the tree or are new files in directories that do
(`plan-names-real-paths`; anything with a slash or a source extension
is checked, since plans create files but do not invent directories). That is all the kernel can
verify about a plan, and it is enough to catch the two ways a plan
misleads: by inventing files, and by being too thin to follow. The plan
is recorded on the task and shown to every later directive as "Plan from
the investigate step", labelled as checked only for real paths, so the
coder treats it as a map rather than a verdict. A question from the
investigator blocks the task the way any question does; a task that is
impossible as stated costs one read of the tree rather than several
attempts at writing. `investigate` is the built-in directive on this
contract; `planned` is the workflow that runs it before `code`.

## Operations

An operation is a command with a timeout, run in the sandbox against the
tree; exit code decides, output tail kept, zero dollars.

An operation may set `overlay = true` to run with the verification
namespace overlaid from the trusted refs, which is how a hidden suite on
`forge-verify` (a Playwright suite, say) runs against the branch without
the coder ever seeing it; the overlay is removed afterwards. It may set
`verifies = true` to make its failure the preceding directive's failure:
the output goes back to that directive as a retry, within its attempts,
instead of failing the task one shot. A verifying operation must follow a
directive.

An operation may set `output = "full"` (default `"tail"`) to keep its
whole stdout and stderr, merged and capped at 1 MB, on its `ops` row
instead of the last 40 lines. Use it for an operation whose entire output
is the point, such as a benchmark or a generator's log; the default tail
is enough for a check whose failure speaks for itself. `output` applies
to operations only.

## Landing

A task's base is the base branch as the push remote has it, fetched at
clone time, so a task started after a landing sees it. Once the last
step passes, the kernel lands the branch, one task at a time per
repository:

1. `integrate`: fetch the base again; if it moved, merge it into the
   branch. A clean merge is committed as Forge. Then run every repository
   check and every hidden suite (`forge-verify` at its current tip and
   `verify/<id>`) on the merged tree, with the checks read from the base
   as it is now. Before landing, a task is judged by `forge-verify` as it
   was when the task cloned its base: a suite that grew meanwhile tests
   features the base never had.
2. `push`: the branch, as before.
3. `land`: fast-forward the base branch on the remote to the branch, and
   fold the task's `verify/<id>` namespace files into `forge-verify` as
   one commit, so the hidden tests accumulate.

A conflict, or a check that fails only with the base merged in, goes back
to the last `code` directive as a retry within its attempts: the current
base is placed in the clone as the local branch `forge/<base>`, the
feedback names the conflicting files or the failing checks, and the
coder merges and fixes. Verification then measures the branch from the
base it merged, and credits a merge commit with what it resolved, not
with what it carried. A landing that cannot finish, such as a base that
keeps moving under it, fails the task with the reason. `--no-land`
leaves the verified branch pushed for a human, which is also what happens
to a task the reviewer demoted or could not finish. A repository with no
push remote is never landed.

A standing hidden test can also be *right* and still stop a task: a
feature the backlog asks for may overturn an assumption an earlier task
pinned. The coder cannot edit those tests, so its honest exit is
`needs_input` of kind `suite`, naming the test and the assertion; the
task blocks and a human decides which is right, by editing the test on
`forge-verify` or rewriting the task.

The kernel applies the same idea to the hidden tests: when a repository
check fails on the implementer's tree and every location it reports lies
inside the verification namespace, the failure is the test author's, not
the implementer's, who cannot see those files. The task rewinds to the
`tests` directive with the check's output, within that directive's
attempts, and the implementer's attempt is not counted.

Two classes. Kernel operations are inserted by the engine and cannot be
listed, omitted, or reordered: `clone` first, `verify` after every
directive, then `integrate`, `push`, and `land` after the last action.
They appear in the trace as rows. User operations are listed in a workflow: `setup` before
the coder starts, a benchmark after it, a generator for derived files. A
user operation may not shadow a kernel name.

Decided 2026-09-10: three things an operation may do beyond gating, each
declared in its file and each enforced by the kernel.

- **It is told the task's facts.** Every operation runs with
  `FORGE_TASK_ID`, `FORGE_WORKFLOW`, `FORGE_STEP`, `FORGE_BASE_BRANCH`,
  `FORGE_BASE_SHA`, `FORGE_BRANCH`, `FORGE_PREV_SHA` (HEAD before the
  preceding directive ran, so an operation can judge that step alone),
  `FORGE_TASK` (the task text), `FORGE_BIN_DIR` (where forge and its
  tools live), `FORGE_HOT_FILES` (the files successful attempts on this
  repository read most, comma-separated), `FORGE_CACHE_DIR` (a shared,
  writable directory under FORGE2_HOME for content-addressed artifacts such
  as the repository map's parsed blobs), and `FORGE_NAMESPACE` (the verification
  directories, space-separated) in its environment, and nothing else of
  Forge's. Each is already recorded on the task; the operation learns
  nothing the trace does not show. This is what lets an operation judge
  the change rather than the tree: `git diff $FORGE_BASE_SHA`.
- **It may change the tree.** An operation with `produces = ["branch"]`
  is mutating. After it exits 0 the kernel commits whatever it changed
  (author Forge, message `forge: <name>`) and verifies the result: L0 on
  the tree alone (clean, protected paths untouched, nothing under the
  namespace; there is no envelope, so no claim rows), then L1 and L2
  exactly as after a directive. The verdict is a `verify` row at the
  operation's seq. A failure fails the task; there is no agent to retry
  and the commit stays on the branch for inspection. If the operation
  changed nothing, nothing is committed and the verify row says so. The
  invariant this keeps is the one that matters: the tree pushed is the
  tree verified. What it buys: a formatter or a generator that runs at
  zero dollars instead of costing a retry at LLM price.
- **It may produce the interface.** An operation with
  `produces = ["interface"]` hands its stdout to the next code directive as
  the interface, replacing whatever a `tests` directive said. A summary
  from an agent is a claim; a list extracted from the tests is a fact. An
  operation that `consumes = ["verify_ref"]` runs not in the clone but in a
  scratch copy of the base commit with the task's verify ref overlaid,
  and the scratch is removed afterwards: the coder's tree never holds the
  hidden tests, and the scratch has no git history. The output is
  recorded on the operation's row and shown by `forge trace`.

An operation may produce only `branch`, `interface`, and `context`; `verify_ref` and
`verdict` are a directive's and the kernel's. A mutating operation
produces a `verdict` for the data-flow check, since the kernel verifies
after it.

Built-in operations, written on first use next to the directives and
never overwritten: `setup` (the repository's setup check), `diff-size`
(fails past a cap on lines and files changed against base; the caps are
the last two elements of `run`), `fmt` (runs the formatter the tree's
layout suggests and commits the result), `interface` (the files under
the namespace, what they import, and the names they call, never the
assertions), `playwright` (the hidden e2e suite from `forge-verify`, run
with the namespace overlaid; verifies), `comments-only` (fails when the
preceding step changed anything but comments and docs; verifies), and
`graph-check` (docs/SYSTEM.md exists, holds a Mermaid block, and names
only real paths; verifies), and `repo-map` (produces `context`: every
source file's declared symbols ranked against the task's words and the
files earlier successful work read most, cut to a budget, by the
deterministic `forge-repomap` tool; shown to the next directive as
"where things are", recorded in the attempt's inputs; `--no-context` on
a task is the control arm). Each is a starting point the operator edits,
and every edit is a new hash with its own numbers.

## Data flow

Each action declares what it consumes and produces from a small
vocabulary: `branch`, `verify_ref`, `interface`, `verdict`, `review`. The validator
checks the chain in order, so `code` after `tests` provably receives an
interface, and an operation placed before the branch exists is rejected at
load, not at three in the morning. The inputs and outputs recorded per
attempt are the runtime half.

## Composition

A step may reference a workflow: `{ workflow = "review-pass" }`. Two
forms, one now and one later.

- **Inline** (now): the child's actions are spliced in at that point at
  task creation, resolved and recorded like everything else. One task, one
  branch, one trace. The kernel still inserts verify after every directive
  regardless of nesting. The reference graph must be a DAG; the data-flow
  chain must still type-check across the splice.
- **Decomposition** (after the integrator): a directive that emits child
  tasks, each with its own workflow, clone, branch, budget, and verdict,
  and a parent step that waits for them and integrates their branches.
  This is how a company works. It needs the merge queue and a budget rule
  that caps the tree, and the inline form is a strict subset of it.

## Built-in workflows

| name | steps |
|---|---|
| `direct` | setup → repo-map → code |
| `tdd` | tests → setup → repo-map → code |
| `docs` | setup → docs |
| `cheap` | setup → repo-map → fix (haiku, 15 turns) → fmt |
| `polish` | setup → repo-map → code → polish |
| `reviewed` | setup → repo-map → code → review |
| `tdd-reviewed` | tests → setup → repo-map → code → review |
| `playable` | setup → repo-map → code → playwright (hidden suite, verifies) |
| `documented` | setup → repo-map → code → document → comments-only (verifies) |
| `mapped` | setup → repo-map → code → graph → graph-check (verifies) |
| `planned` | setup → repo-map → investigate → code |

Each carries `[meta]` saying when to use it and when not. What each costs
and achieves is measured, never declared; see docs/WORKFLOWS.md.

## In the file

```toml
name = "tdd"
description = "hidden tests first, then code"
steps = [
  { action = "tests" },
  { action = "setup" },
  { action = "code" },
  { workflow = "review-pass" },
]

[meta]
use_when = "..."
avoid_when = "..."
requires = ["[verify] namespace in forge.toml", "a check named test"]
```

Verify and push are not in that list, on purpose.
