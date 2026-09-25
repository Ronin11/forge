# Execution, flow, and judgment (design note, 2026-09-25)

*Where a step runs, what happens when it fails, and where the model is
allowed to be. Decided with Nate on 2026-09-25 after the first real
customer interview (desk-assistant, task 669) made it plain that Forge's
output for an end user is a Forge workflow, not software shipped
elsewhere. Supersedes the "not built" line on conditionals in
docs/WORKFLOWS.md for run workflows only.*

## The lines that stay hard

Software and automation are both the input and the output of Forge, so
the boundary between "what Forge builds" and "what Forge runs" is not
worth drawing. Three other lines carry the weight, and they do not move:

1. **Where a step runs.** A directive runs in an executor the kernel
   controls, with bounded egress and seeded credentials; an effect runs
   on the host through a declared operation. The executor is
   configurable (below); the fact that there is one is not.
2. **What is verified.** A build workflow's landing passes its checks.
   A run with an effect has a passing fixture before it is enabled. A
   claim without evidence is a failure. Verification and integration
   are never steps a workflow lists, because they are never optional.
3. **Where judgment lives.** Inside a step, never in the control flow.
   A directive reports what it found as data; a table or an edge
   decides what happens next. No model call ever chooses a route.

And one rule of composition, enforced, not preferred:

4. **An operation unless judgment is genuinely needed.** A script is
   consistent, fast, and free; a model is none of those. Parsing a
   date, fetching mail, filling a template, comparing two numbers are
   operations. Reading an email nobody has seen before and deciding
   what it asks for is a directive. Every directive step in a run
   workflow carries `judgment = "<one sentence saying what a script
   cannot do here>"`; the lint refuses one without it, and `forge
   stats` shows each workflow's directive share of cost so the drift
   toward the hammer is a number.

## Executors

Today the only executor is bwrap on this host (`src/sandbox.rs`), and
it is the security boundary: tmpfs home, the worktree bound, an egress
proxy, private copies of the CLIs' credentials, package caches overlaid.
Some work cannot run there: a step that has to touch the dev server, a
different OS image, a machine a second operator runs. So the executor
becomes a contract with backends:

```toml
# a project's forge.toml, or a deploy target's record
[execution]
backend = "bwrap"        # the default; "host" (unsandboxed, today's FORGE_SANDBOX=0), "container", "ssh"
# backend = "ssh"
# host = "dev.home"
# user = "forge"
```

The contract every backend meets, and `forge doctor` reports which
guarantees each one actually provides on this machine:

- the worktree is present and writable, nothing else of the operator's
  home is visible;
- egress is bounded to the model endpoints and the repository's
  declared hosts, or doctor says it is not;
- the agent CLIs are reachable and signed in, with credentials the
  attempt cannot exfiltrate to the record;
- the kernel can run the repository's checks under its own control.

That last point is the one that matters. A remote step's own report of
"the tests passed" is a claim; the verdict is what the kernel ran. When
a backend cannot give the kernel that (a host it can only `ssh` a
command to), the run is recorded as unverified with the reason, the
same state a review that could not finish leaves a branch in.

Two consequences an executor config must carry rather than stumble
into. A subscription login is a per-machine OAuth session, so a remote
backend either has its own login or uses an API-key provider (the
identity decision in docs/NATE.md, forced by the first remote step).
And bwrap stays the default because it is the lightest boundary that
meets the contract; a container earns its place for a different image,
not as a habit.

## Verifiable inside, optional outside

An internal flow (a build workflow on any repository, and every run
workflow Forge itself depends on) always has a verifiable output: a
landing that passed its checks, a run whose check exited 0.

An external flow, an automation built for a person, may declare no
check. Its runs are then recorded as unverified, which costs it the one
thing a check buys: the economist and the stats can learn nothing from
it. That is the right price. But an external flow with an effect and no
check would act on the world with nothing able to say it was wrong, so
the fixture mechanism (docs/WORKFLOWS.md, "Fixtures, and `forge job
test`") becomes a gate: a run workflow with any `effect` step cannot be
enabled until `forge job test` has passed on a fixture. Optional
verification, mandatory rehearsal.

## Outcomes, then edges

A directive already answers against a schema. Add named outcomes to an
action's contract:

```toml
# actions/triage-mail.toml
outcomes = ["reply", "forward", "uncertain", "nothing-to-do"]
```

The model picks one as a field of its structured result. The lint
checks that every outcome an action can emit has somewhere to go, and a
step's edges say where:

```toml
steps = [
  { action = "fetch-mail" },                       # operation
  { action = "triage-mail", judgment = "an unknown sender's ask is not a pattern a rule can match",
    on = { uncertain = "ask-april", nothing-to-do = "end" } },
  { action = "draft-reply", judgment = "the wording answers a question the template does not anticipate" },
  { action = "send-mail", effect = "message" },
  { action = "ask-april", effect = "message" },
]
```

`on` maps an outcome, or `failure`, to a later step, an earlier step, or
`end`. Success without an `on` entry is the list order, as today. A
backward edge is a loop, bounded by the step's attempt cap, never by a
counter of its own. Joins and parallel branches are not built until a
workflow needs one; the task-level `after` already fans work out.

Build workflows keep the list. The kernel's five failure rules (a verify
failure returns to the directive it judges; a tests-namespace failure
returns to the tests step; a review demotion with a reproduction files a
follow-up task; an environment need re-runs the step; a spent budget
ends the run with the branch kept) handled every failure of the week
this was decided, and they stay as the defaults a run workflow's edges
may override but never remove: an edge can say where to go after the
kernel has judged; it cannot route around the judging.

What this costs, honestly: the run is today a cursor over a list with
three outcomes (next, again, end). Edges need a node id per step, a
resolved graph frozen on the task, and `forge stats --by-step`, the
profiles, resume and `forge trace` keyed by node rather than position.
That is the flow engine Forge 1 built, confined to the failure path of
run workflows.

## The directive library

Forge 2 already reused Forge 1's word: a directive is an action with a
contract, a schema, and a prompt field. Two things Forge 1's library
had are worth bringing back, and nothing else:

- **Content in git with a hash the record keys on.** The workflow
  catalog already does this for workflows; prompt content joins it, so
  an attempt records the exact text it was given and `forge stats`
  compares versions of a prompt the way it compares versions of a
  workflow.
- **Includes.** A shared standard (the untrusted-data sentence, a
  project's house rules) lives once and is included by name, with the
  include's hash in the attempt's inputs, so an A/B on a fragment
  attributes cleanly.

Left behind on purpose: the persona/mode/routine layering, which was
more structure than its numbers justified, and conditionals in content.
Teaching is selection, not branching; that decision held in Forge 1 and
holds here.

## Order

Each depends on the one before it:

1. The executor contract and config, with `bwrap` and `host` as the
   first two backends, doctor reporting the guarantees, and the
   unverified-when-remote rule.
2. The `ssh` backend against dev.home; then a container backend when a
   different image is actually needed.
3. Outcomes in action contracts, the `judgment` field and its lint, the
   directive share in stats, and the fixture gate for effects.
4. Failure edges for run workflows, on the failure path only.
5. The directive library: hashed content, includes.
6. The editor. A visual composer is the last thing, once the model it
   edits has stopped moving; until then the TOML editor with live lint
   on `/workflows/<name>` is the editor.

## Not built

Edges on build workflows. Joins. A counter-bounded loop. A backend that
runs a directive with no egress bound and no doctor line saying so. A
model call anywhere in the control flow.
