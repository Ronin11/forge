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

Data, not code. The set of step kinds is closed and small.

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

## Not built

Workflow conditionals or variables. Per-step prompts as configuration. A
step that runs outside the sandbox. A step the kernel cannot verify
afterward. A workflow editor.
