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

The executor contract (`src/executor.rs`) supports bwrap, host, and SSH.
Bwrap (`src/sandbox.rs`) is the default security boundary: tmpfs home, the worktree bound, an egress
proxy, private copies of the CLIs' credentials, package caches overlaid.
Some work cannot run there: a step that has to touch the dev server, a
different OS image, a machine a second operator runs. So the executor
becomes a contract with backends:

```toml
# a project's forge.toml, or a deploy target's record
[execution]
backend = "bwrap"        # the default where bwrap is installed, else host; "host" (unsandboxed, today's FORGE_SANDBOX=0), "container", "ssh"
# backend = "ssh"
# host = "dev.home"
# user = "forge"
```

`[execution]` is read from the trusted base, like `[sandbox]` egress.
Each attempt records `executor` and `guarantees` in its inputs. Host
runs directly with the agent environment: it provides kernel-controlled
checks, but no private worktree, bounded egress, or private credential
seeding. `forge doctor` reports an `executors.<backend>` row and warns
that host egress is unbounded. `FORGE_SANDBOX=0` remains an operator
override selecting host. The SSH backend accepts `host` and an optional `user`, copies the tree
with rsync to a temporary directory, executes argv with the executor environment,
and copies edits and commits back even after a nonzero exit. Remote CLIs must be
installed by the operator; doctor probes them over SSH and warns about
subscription credentials without `api_key_env`. SSH provides no isolation or
kernel-controlled checks: attempts end unverified with
"remote executor: the kernel could not run the checks itself".
The container backend is not implemented.

The contract reports these guarantees as data, and `forge doctor` reports
which each backend provides on this machine:

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

## The judgment tier

An operation unless judgment; typed judgment before prose. When a step
truly needs judgment, most of the time it needs a *classification*, not a
paragraph: which of these outcomes, how sure. A model that answers in
prose is the wrong tool for that. TypeSafe's Jev is a System One model:
state plus typed questions in; a `choice`, `score` or `noul`, with its
probabilities and a confidence, out, never text. Through Cloudflare
Workers AI it took 687 ms and 500 input tokens for a short email
(verified by hand, 2026-09-26), at $0.042 per million input tokens and
free output. TypeSafe serves the same request directly (the operator's
account, 2026-09-28): `POST https://api.typesafe.ai/v1/systemone` with
`Authorization: Bearer <key>`, the body `{"model", "state", "questions"}`,
the response `{"model", "answers", "usage"}` at the top level.

```toml
# config.toml; every key but `runner` is its default
[providers.jev]
runner = "jev"
backend = "auto"                             # auto | cloudflare | typesafe
base_url = "https://api.typesafe.ai/v1/systemone"
api_key_env = "TYPESAFE_API_KEY"             # names the variable, never holds the key
model = "jev-latest"
price_usd_per_million_input = 0.042
price_usd_per_million_output = 0
# Cloudflare Workers AI, used first until its credits run out:
cloudflare_url = "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/run"
account_id_env = "CLOUDFLARE_ACCOUNT_ID"
cloudflare_api_key_env = "CLOUDFLARE_API_TOKEN"
cloudflare_model = "typesafe/jev"
```

Two hosts, until the Cloudflare AI Gateway's remaining credits are used
up. Under `backend = "auto"` a call goes to Cloudflare first (when its
token's variable is set), wrapped the way Workers AI takes it
(`{"model", "input": {"state", "questions"}}`, answers read from
`result.result`). When Cloudflare answers 402 (the gateway's
"Insufficient balance"), the process asks TypeSafe instead, stays on
TypeSafe for the rest of its life, logs one line saying so, and writes
`jev-cloudflare-exhausted` (the date) beside the worker's credentials
drop-in, `$XDG_CONFIG_HOME/systemd/user/forge-worker.service.d/` (else
`~/.config/...`), so every later process goes straight to TypeSafe.
`backend = "cloudflare"` or `"typesafe"` forces one host, and never
switches. Once the marker exists, the Cloudflare code, keys and these
paragraphs are deleted (a later task). A table written before TypeSafe,
whose `base_url` names `{account_id}`, keeps its `base_url`, `api_key_env`
and `model` as Cloudflare's.

A 429 or 529 is retried with exponential backoff (one second, doubling)
up to three times inside one call; past that the call fails as the
provider refusing it (`rate_limited`, a five-minute window-style hold),
not as the step's own failure. `forge doctor`'s `providers` row names
each jev provider's host and warns when a variable its key (or, while
Cloudflare is in use, its account id and token) is read from is unset.

An action opts in by describing its outcomes (the list form still works,
each name then being its own description) and may add questions and a
confidence floor:

```toml
# actions/triage-mail.toml
confidence_below = { 0.6 = "uncertain" }   # a judgment under 0.6 routes as `uncertain`

[outcomes]
reply = "a person wrote a question that needs an answer"
forward = "the message is for someone else"
nothing-to-do = "a newsletter, a receipt, noise"

[[questions]]
name = "urgency"
type = "score"                              # choice | noul | score
instructions = "How soon does this need an answer?"
criteria = ["low", "medium", "high"]
```

The request's `state` is the step's inputs, the same text a chat
directive gets (the input document and earlier step outputs); the outcomes
become one `choice` question named `outcome`, its criteria the
descriptions; each `[[questions]]` entry is asked beside it. The result is
the directive's structured envelope: `outcome` (after the floor),
`confidence`, `probabilities`, and any other question's answer under
`answers`. Each answer type is read the same way: a `choice` names its
label; a `noul` comes back as a bare `{"type":"noul","noul":0.22}`, true at
0.5 or above, with confidence `|noul - 0.5| * 2` and probabilities
`{true: noul, false: 1 - noul}`; a `score` is the most probable level of
its `probabilities`, named through its `legend`, and its criteria go out as
an array of level names (an object's keys are sent in that form). The floor's names are outcomes like any other, so an edge
routes on them (`on = { uncertain = "ask-april" }`); the choice the floor
overrode stays in the envelope as `choice`. The step's row in `job_steps`
records the probabilities (`forge job show --json`'s
`steps[].probabilities`) beside the outcome. Cost is the usage tokens at
the provider's price; the log carries the request and the response.

The runner is HTTP only and has no tools, so it is refused for anything
but a job's directive step whose action declares outcomes; the step's
`schema` is not what it answers against, the outcomes are. It runs on the
host, never in a sandbox, so it adds no rule to the egress allowlist.

### Measuring it: `forge eval jev`

Before anything routes through Jev, `forge eval jev --provider jev` scores it against
what the record already knows. It changes no routing; it measures. Three sets are
pulled from the store and each item is sent to the provider as one question named
`outcome`:

1. **Concierge decisions**: every message the concierge sorted as state, labeled
   with the kind it recorded: each of its own runs (whose plan is the decision), the
   `decisions` it answered (`answered_by = 'concierge'`) and every task filed through
   it (`concierge_json`), each message once; a `choice` over `request`, `question`, `need`, `unclear`, whose criteria
   are docs/INTAKE.md's definitions.
2. **Review demotions**: each demotion's text as state, one `noul` (criteria
   `true` and `false`, read as `yes` and `no`): names a
   reproducible defect with a command or steps. Labeled `yes` when the
   demotion-as-task rule filed a follow-up (or the answer was "do it as stated"),
   `no` when it blocked as a question.
3. **Task size**: each landed task's text as state, a `score` over `small`, `medium`,
   `large`, labeled from lines changed (added plus deleted, base to landing): small
   up to 50, medium up to 300, large above. It is the proxy for "which workflow".

Per set the report gives accuracy against the label, a calibration table (confidence
decile against accuracy), mean latency and total cost from usage. It is written to
`docs/research/jev-eval-<date>.md` (`--out` overrides) and printed. `--record FILE`
saves the pulled sets and `--fixture FILE` replays such a file instead of reading the
store (`{"concierge": [{"state", "label"}], "demotions": [...], "size": [...]}`),
which is how the e2e test runs it against a fake endpoint. `--limit N` judges only
each set's newest N.

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

## The agent login

The claude login (`<config dir>/.credentials.json`) is the kernel's, not any one sandbox's
(`src/login.rs`). OAuth refresh tokens rotate, so a sandbox that refreshes its private copy leaves
the host file's refresh token dead, and the next host-side refresh empties the file. So:

- after every attempt, and before seeding any launch, a private copy whose `expiresAt` is later than
  the host file's is copied back over it (a sibling file renamed into place, under a lock);
- a host login within 30 minutes of `expiresAt` is refreshed on the host first, one launch at a time,
  by a one-token `claude` probe through the attempts' own lean argv;
- a host file with an empty or missing token is never seeded: the launch is a provider refusal
  (held, not counted as an attempt), and `forge doctor` fails the `anthropic` row with
  "run `claude login`". That row also shows the token's `expiresAt` and any write-back in the last 8 hours.
- a login the provider itself refuses (claude's "Failed to authenticate", an HTTP 401 or 403 API
  error, codex's and copilot's equivalents, or one on stderr alone) is a provider refusal like a
  spent window: the attempt is refunded and the task requeued on the base and hidden suite it had,
  its attempt count untouched. The provider is held (`src/login_hold.rs`), and the hold names no
  time: it ends when a one-token probe answers. The worker probes a held provider when it starts
  and every 10 minutes after, each probe recorded in `provider_probes` with its cost, and
  `forge doctor` probes it too, so logging in and running `forge doctor` releases it. Until then
  `forge doctor` fails the provider's row ("anthropic: login expired since 23:48; run claude login
  as the operator, then forge doctor"), and the notify and signal plugins carry the hold once, as
  a `provider_held` event, not once per attempt.
