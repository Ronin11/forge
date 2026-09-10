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

Directives are code-defined for now: `code` and `tests`. Their files carry
parameters and a description, not prose, and a directive file with any
other name is rejected by the validator until custom directives exist. A user-authored
directive arrives when a workflow request shows the need, with one rule:
it names the operations that verify its output. The Forge 1 directives
library was prose agents could also bypass; a directive here is bound to
a kernel-enforced contract, which is the difference.

## Operations

An operation is a command with a timeout, run in the sandbox against the
tree; exit code decides, output tail kept, zero dollars.

Two classes. Kernel operations are inserted by the engine and cannot be
listed, omitted, or reordered: `verify` after every directive, `push`
after the last action, `integrate` when it exists. They appear in the
trace as rows. User operations are listed in a workflow: `setup` before
the coder starts, a benchmark after it, a generator for derived files. A
user operation may not shadow a kernel name.

## Data flow

Each action declares what it consumes and produces from a small
vocabulary: `branch`, `verify_ref`, `interface`, `verdict`. The validator
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
cost_factor = 2.5
```

Verify and push are not in that list, on purpose.
