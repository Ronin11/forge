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
workflow: `use_when`, `avoid_when`, `requires`. Never a cost: cost and
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

The set of step kinds is closed and small.

| name | steps | what the coder sees |
|---|---|---|
| `direct` | code | the task |
| `tdd` | tests, code | the task and the interface the tests expect, never the assertions |

Step kinds:

- `code`: an agent in a per-task clone of the base branch, writing the
  change. May not touch protected paths or the verification namespace.
- `tests`: an agent in its own clone, writing tests only inside the
  verification namespace. The tests must fail on the base commit (red)
  and be committed to `verify/<id>`. Its summary is the interface handed
  to the coder.

Kernel steps, always, not listed:

- verify: L0 on the step's tree; then the verification namespace is
  overlaid from the trusted refs (`forge-verify` for standing suites,
  `verify/<id>` for the task's tests) and L1 and L2 run; then the overlay
  is removed so the next attempt starts blind.
- push: the verified branch, by explicit refspec.
- integrate: the merge queue, when it exists.

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

Not Temporal, not Ansible. The engine is two hundred lines and the
workflows are one to three steps on one machine with state in SQLite;
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
