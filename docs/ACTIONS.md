# Actions

Decided 2026-09-10. A workflow is an ordered list of actions. An action is
a directive (an LLM step) or an operation (a deterministic step). The line
between them is the verification boundary: a directive's output is a
claim, an operation's output is a fact, and nothing a directive produces
reaches main without passing through an operation.

## Files and identity

One file per action in `<FORGE_HOME>/workflows/actions/<name>.toml`, next
to `<FORGE_HOME>/workflows/<name>.toml` for workflows. The directory is a
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

A directive is a prompt template, a capability set, and a contract. The
capability set is the same for every directive on the claude runner and
is fixed at launch: Bash, Read, Edit, Write, Glob and Grep, plus the
StructuredOutput tool the result schema adds, and nothing else: no MCP
server, no skill, no plugin, none of the operator's user settings (the
launch passes `--strict-mcp-config --disable-slash-commands
--setting-sources project,local --exclude-dynamic-system-prompt-sections`;
see `agent::claude_argv`). A job's directive step gets no tools at all.

- Capabilities: what the agent can see and touch. Whether the verification
  namespace is hidden, whether the acceptance checks are visible, which
  paths it may write, the model, turns, and timeout.
- Contract: the envelope it must produce, and the verification rules
  specific to its output. `tests` adds namespace-only and red-on-base;
  `code` adds namespace-untouched.
- The prompt is the smallest part.

The model's result contains a summary, checks run, claims with evidence,
and any request for input. It is not asked to report `changes[]`: for
every provider the verifier derives that field from git and stores it
in the envelope. There is no model-reported file list to reconcile.

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
`tests/e2e/contracts.rs`'s
`a_readme_rule_that_contradicts_the_task_stops_at_investigate_before_any_code_runs`
is the standing proof of this: a fixture repository whose README states a
rule, a task that asks for the opposite, and an investigate directive
that reads both end the task blocked on a question quoting the
contradiction, with the code step never given an attempt — a
contradictory brief stops before any code runs, before money is spent.

`interview` is the plan contract's other directive: the second
conversation with a person who does not think in workflows (see
docs/INTAKE.md). It is read-only like `investigate` — no writes, and the
sandbox binds nothing writable but the scratch — but its summary is
never a repository plan: on any given turn it is either the next plain
question, or, once its checklist is satisfied, the brief plus the
confirmation question, or, if the person asked to stop, a plain sentence
saying so. None of that is held to `plan-substantive` or
`plan-names-real-paths`; the kernel verifies `interview` on `untouched`
and a structured result alone, the same way it holds every other
contract to only what it can check. The brief is carried forward as
`t.plan` to each next turn of an intake task, and the last turn — once
the person confirms it — re-emits the same brief with `confirmed:true`
as that turn's own plan. `intake` is the workflow that runs it after
`setup`.

`concierge` is the plan contract's third directive: the front door, read
only like `investigate` and `interview`, that sorts a customer message
into a `request`, a `question`, a `need`, or `unclear` (see
docs/INTAKE.md, "The front door is not the interview"). Its summary is a
one-line JSON decision, never held to `plan-substantive` or
`plan-names-real-paths` for the same reason `interview`'s is not — it
describes nothing in the tree. `forge ask <project> <message> [--from
<contact>]` is its only caller: it runs the `concierge` workflow (`setup`
then `concierge`) to completion, reads the decision back off `t.plan`,
and acts — files the task, prints the answer, files an intake task, or
blocks a small placeholder task with the question addressed to the
contact — recording the decision as a `concierge_json` column on the task
it produced, or a `decisions` row (`answered_by` "concierge") for an
answer.

A plan can also become an initiative's tasks instead of one task's code.
`forge initiative from-plan <task id> [--outcome <text>]` reads a
finished task's recorded plan (`t.plan`), creates an initiative in the
task's project (outcome defaulting to the task's own text when
`--outcome` is not given), and files one task per plan item — the plan's
paragraphs, blank-line separated, in order — each depending on the one
before it, against the task's repository, with the plan item as the new
task's text and the originating task recorded as a reference of kind
`plan` on each. Refused when the task has no plan.

The plan contract's action can do this itself, mid-run: a directive
naming it may set `file_into_initiative = true`. When the task has an
initiative id, the engine files the plan's items as sibling tasks in
that same initiative right after this step, exactly as `from-plan`
would, instead of continuing to the `code` step in this task; the task
then ends succeeded, with a reason naming how many it filed. Without an
initiative id the flag does nothing and the workflow continues as
written, since there is nothing to file into.

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

### Checks and known fixes

`forge.toml`'s `[checks]` table is name → argv, run as L1 in the sandbox
against the tree, read from the trusted base commit rather than the
branch under test so an attempt cannot change what it is verified
against (see `docs/SYSTEM.md`, `src/config.rs`). `[checks.fixable]` names,
for a check that only checks (`fmt = ["cargo", "fmt", "--all",
"--check"]`), the command that fixes what it flags (`fmt = ["cargo",
"fmt", "--all"]`); every key must already be a `[checks]` entry.

Every check runs with the task's facts in its environment, the same
four an operation gets first (below, "It is told the task's facts"):
`FORGE_TASK_ID`, `FORGE_BASE_SHA`, `FORGE_START_SHA` (HEAD before this
attempt began) and `FORGE_BRANCH`. That holds for a repository check
(L1), a task's own `--check` command (L2), a fix command, and the
tests contract's red-on-base run in its scratch tree, so a check can
judge the change rather than the tree: `git diff --stat
"$FORGE_BASE_SHA"`, or a task check that asserts the branch still
builds on the base it was queued against. One builder in
`src/operation.rs` (`task_facts`) produces the list for checks and
operations alike. The hidden-test overlay and the namespace rules are
unchanged by it.

When a `code` attempt's checks fail and every failing one is named in
`[checks.fixable]` — never `setup`, whose failure means nothing else ran
— the engine runs those fix commands in the worktree before any retry,
commits the result as Forge naming the checks fixed, and runs the checks
once more. Passing now ends the attempt succeeded, exactly as if the
agent had gotten it right the first time; still failing lets the ordinary
retry path begin. Either way the fix run is its own `known-fix` row on
the attempt's operations, with the diff it committed.

A check command is trusted to report pass or fail, never to touch the
tree it is judging: the verdict names the one commit L1 and L2 ran
against, and a check that commits, stages, or leaves a tracked file
modified fails L0's `candidate-unchanged` row, naming the commit it was
supposed to be judging and the one it left behind instead — even if
every check it ran otherwise passed, so a check that quietly slips
`forge.toml` or a protected path into the tree cannot land it under a
green `forge.toml-untouched`. The one commit allowed to move the tree
mid-verify is the known-fixes commit above: it runs outside the checks,
and the re-run it triggers is judged as the new candidate, not the old
one.

## Landing

A task's base is the base branch as the push remote has it, fetched at
clone time, so a task started after a landing sees it. Once the last
step passes, the kernel lands the branch, one task at a time per
repository:

0. Before any of that: the worktree's HEAD must still be the `end_sha`
   of the task's last succeeded attempt. A mismatch refuses landing with
   a reason naming both shas rather than fast-forward the base to a
   commit no check ever ran on.
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
  `FORGE_TASK_ID`, `FORGE_BASE_SHA`, `FORGE_START_SHA` (HEAD when the
  operation began; the four every check gets too, above), `FORGE_BRANCH`,
  `FORGE_WORKFLOW`, `FORGE_STEP`, `FORGE_BASE_BRANCH`, `FORGE_PREV_SHA`
  (HEAD before the preceding directive ran, so an operation can judge
  that step alone),
  `FORGE_TASK` (the task text), `FORGE_BIN_DIR` (where forge and its
  tools live), `FORGE_HOT_FILES` (the files successful attempts on this
  repository read most, comma-separated), `FORGE_CACHE_DIR` (a shared,
  writable directory under FORGE_HOME for content-addressed artifacts such
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
only real paths; verifies), `egress-probe` (fails unless the attempt's network is bounded: a
connection straight to a public address is refused, the egress proxy
answers with its policy, and a host on no allowlist gets a 403 both
tunnelled and plain; verifies; it fails outright unsandboxed, where
there is no proxy; see docs/SYSTEM.md, `src/egress.rs`), `repo-map` (produces `context`: every
source file's declared symbols ranked against the task's words and the
files earlier successful work read most, cut to a budget, by the
deterministic `forge-repomap` tool; shown to the next directive as
"where things are", recorded in the attempt's inputs; `--no-context` on
a task is the control arm), `repo-graph` (writes `forge graph`'s own
document — docs/LATER.md, "The code visualiser" — for the attempt's tree
to `$FORGE_CACHE_DIR/graph.json`, fresh on every run, so a landing always
leaves an up to date graph behind for a client to read; deterministic,
no model), `deploy-command` (the generic deploy
method `forge deploy` runs, outside of any task: rsyncs the landed tree
to a target's host and dest over ssh, runs its command, then its check,
both over ssh unless the host is `local`; see docs/DEPLOY.md),
`deploy-user-service` (`deploy-command` plus restarting a user-level
systemd unit on the host with `systemctl --user restart <unit>` and
waiting, bounded, for `systemctl --user is-active <unit>` to report
active, before the check; see docs/DEPLOY.md), `deploy-self` (Forge on
this machine: builds the landed tree with `cargo build --release
--workspace` into the registered checkout's target directory, keeping the
previous binaries in `target/release/previous`, restarts `forge-web` and
`forge-portal`, runs the check, by default a token-authenticated GET of
the web client's `/tasks` expecting 200, and last asks `forge-worker` to
restart with `--no-block` so it drains first; a failed build or check
restores the previous binaries and leaves the worker alone; see
docs/DEPLOY.md, "Deploying Forge itself"), and `deploy-static`
(rsyncs a built directory to a target's host and dest, then the check:
the target's own check command if it declared one, else this method's
default of fetching a `url` arg over curl, requiring HTTP 200 and, when
a `marker` arg is given, that string in the body; see docs/DEPLOY.md),
`deploy-smoke` (after a target's check passes, opens its declared
smoke url in headless Chromium through Playwright, records console
errors, failed requests, the title and a screenshot, and fails on a
console error or a failed request to the url's own origin; see
docs/DEPLOY.md), and `provision-hetzner` (what `forge provision` runs:
creates a same-named firewall allowing 22, 80, 443 and icmp if absent,
`hcloud server create`s the box, waits for it to report running, prints
its ipv4, and writes an ssh-config fragment for the operator to append
to their own `~/.ssh/config`; see docs/DEPLOY.md, "Provisioning").

A job's steps (docs/JOBS.md) are operations too, and five built-in ones
perform its effects, each honouring `FORGE_DRY_RUN` by logging to
`FORGE_EFFECT_LOG` instead of acting: `write-file` (writes
`FORGE_INPUT_CONTENT` to `FORGE_INPUT_PATH`, the `file` effect),
`append-row` (appends `FORGE_INPUT_ROW` to the table
`FORGE_INPUT_TABLE`, the `row` effect), `http-post` (posts
`FORGE_INPUT_BODY` to `FORGE_INPUT_URL`, the `http` effect), and two
that both perform the `message` effect: `send-signal` (sends
`FORGE_INPUT_TEXT` to `FORGE_INPUT_CONTACT` through signal-cli) and
`send-sms` (the same, over Twilio, from `TWILIO_FROM`; a Twilio error
response — 30034, an unregistered 10DLC sender, notably — exits
non-zero quoting its own code and message, docs/PLUGINS.md, "twilio").

The same tool's `forge-repomap edges <root>
[--cache DIR]` subcommand prints the structure layer of the code
visualiser: one JSON document of every source file the extractor table
handles as a node (`path`, `lang`, `symbols`) and every import that
resolves to another file in the tree as an edge (Rust `use crate::...`
and `mod x;`, TypeScript/JavaScript relative imports and requires,
Python `import`/`from ... import`, and Go imports within the module
path from `go.mod`); imports that don't resolve inside the repository
are dropped. It reuses the same per-language extractor table and
content-addressed cache as symbols, keyed by blob hash, so an unchanged
file costs nothing on the next run. Each is a starting point the
operator edits, and every edit is a new hash with its own numbers.

## Assessment

Forge knows the true cost of a landed piece of work, including what it
cost later, but that number lags: it needs later tasks to land near it
before delayed cost and follow-on repair show up. `assess` is a fast
proxy, in place the moment a task lands.

`assess` is a directive on the `plan` contract, read-only like
`investigate` — no writes, and its run never blocks the task or changes
its state. It is given the landed diff (`git diff base..landed`) and the
task text, and returns a structured result: a `score` from 0
(unmaintainable) to 10 (excellent), and `findings`, each a `path`, one
sentence, and a `severity` of `notable` or `concern`. Its provider is a
role like the others (`[roles] assess`, default `anthropic`).

Unlike every other directive, `assess` never sits in a workflow's `steps`
list. A workflow opts in with its own top-level `assess = true` (default
false); of the built-ins, only `reviewed` and `tdd` set it. After a
landing on such a workflow — automatic or by `forge land` — the kernel
runs `assess` once against the landed diff and stores the row on
`assessments` (`task_id`, `score`, `findings_json`, `model`, `provider`,
`cost_usd`, `created_at`). A run that fails — the agent errors, its
result does not fit the schema, or it touched the tree — is logged and
ignored: no row, no effect on the task.

A task's most recent assessment, once it has one, surfaces wherever the
task itself does: `forge show` prints an `assess` line (score and
finding count) and a `finding` line per finding, under the lineage;
`forge trace --json` carries the same score, findings, model, provider
and cost as an `assessment` object (`null` if it never ran; see
docs/CLIENT.md, "`TraceDoc`"); `forge initiative report` adds a `score`
to each of the initiative's tasks; and the web task view shows both.
`forge stats` never shows a task's own score, but does judge the proxy
itself: `forge stats --quality` prints Spearman's rank correlation
between score and each delayed-cost measure (churn, repair cost), over
landed tasks that carry both, with the count of tasks each rests on —
see "Delayed cost" in docs/CLIENT.md, "`StatsDoc`".

## The deploy look

A deterministic check proves a port answers; it does not prove the page
is fit to show anyone (see docs/DEPLOY.md, "A deterministic smoke
step"). `deploy-look` is the last, human-shaped check: after the smoke
step, a read-only agent looks at the screenshot it took the way a
person opening the site would.

`deploy-look` is a directive on the `plan` contract, read-only like
`investigate` — no writes. It is given the deploy's full-page screenshot
(a path it is told to read as an image), the page's title, the smoke
step's console-error and failed-request lists, the target's own url, and
the project's purpose, and returns a structured result: `ok` (true or
false) and `findings`, each a `severity` of `blocking` or `notable` and
one sentence, judging only what the screenshot shows — error text, a
placeholder or missing image, an empty map or list where content is
expected, broken layout, developer copy left on the page.

Like `assess`, `deploy-look` never sits in a workflow's `steps` list:
`forge deploy` runs it itself, once, as its own last step, whenever the
target declares a smoke url and the smoke step leaves a screenshot to
look at. Its verdict lands on the deploy row (`look_ok`, `look_json`),
shown by `forge deploy log` and the initiative report. A blocking
finding fails the deploy exactly like a failed check: rollback and the
human rung (docs/DEPLOY.md, "Rollback and the human rung") both apply,
the finding's sentence named in the question. A run that fails on its
own — the agent errors, or its result does not fit the schema — is
logged and ignored, exactly like a failed `assess` run: no effect on the
deploy beyond that.

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

| name | steps | the record (all tasks, 2026-09-21) |
|---|---|---|
| `direct` | setup → repo-map → code | 101 tasks, 45 landed (45%), 39 failed, 17 withdrawn; $2.08 per landed. On Anthropic 78% (45 of 58); on the local model 0 of 43. **Default**, for small tasks on a repository with strong checks |
| `tdd` | tests → setup → repo-map → code | 61 tasks, 30% landed; $3.38 per landed |
| `docs` | setup → docs | 11 tasks, 82% landed; $0.67 per landed |
| `cheap` | setup → repo-map → fix (sonnet, 25 turns) → fmt | 18 tasks, 22% landed; $0.88 per landed |
| `polish` | setup → repo-map → code → polish | no figures in the record read |
| `reviewed` | setup → repo-map → code → review | 276 tasks, 188 landed (68%); $4.74 per landed. The choice for anything larger, for weak checks, or with no one to answer |
| `tdd-reviewed` | tests → setup → repo-map → code → review | no figures in the record read |
| `playable` | setup → repo-map → code → playwright (hidden suite, verifies) | no figures in the record read |
| `documented` | setup → repo-map → code → document → comments-only (verifies) | no figures in the record read |
| `mapped` | setup → repo-map → code → graph → graph-check (verifies) | no figures in the record read |
| `planned` | setup → repo-map → investigate → code | 7 tasks, 43% landed; $1.99 per landed |
| `intake` | setup → interview | no figures in the record read |
| `concierge` | setup → concierge | no figures in the record read |

Each carries `[meta]` saying when to use it and when not. What each costs
and achieves is measured, never declared; see docs/WORKFLOWS.md.

### The `direct` verdict

Read from `forge stats`, `forge stats --quality`, `forge workflows` and
the store on 2026-09-21. `direct` stays the built-in default, and it is
the right default only where the task is small and well specified and
the repository's checks are strong. Everywhere else `reviewed` is the
better choice, and `direct` remains available by name.

The rate `forge doctor` warns about, 18 of 50 verified (36%, 95% upper
bound 50%), is not a measure of `direct`. It is the local-model
experiment: of `direct`'s 101 tasks, 43 ran on the local model
(devhome) and none landed, while the 58 that ran on Anthropic landed 45
(78%). `reviewed` has run only on Anthropic, 276 tasks, 188 landed
(68%). Doctor's learning line is to be split by provider so the warning
stops mixing the two; that is a separate task.

On Anthropic, then, `direct` lands more often than `reviewed` and at a
lower cost: $2.08 per landed task against $4.74 in `forge stats`, and a
true cost per landed of $4.40 against $7.93 in `forge stats --quality`
(current hashes; 36 and 177 landings). The price is on the other two
measures. A landing on `direct` broke the base 3% of the time against
1% for `reviewed`, neither was repaired afterwards (0% each), and
`direct` costs more than twice the human attention per landing: 2.11
events per landed against 0.91, its 36 landings drawing 17 answers to
questions and 17 withdrawals. The answers are the questions a review
would tend to settle before an operator is asked; that is a reading of
them, not a measured figure.

By repository the split follows the strength of the checks:

| repository | `direct` | `reviewed` |
|---|---|---|
| forge (the kernel, about 500 tests) | 53 tasks, 75% landed | 223 tasks, 71% landed |
| equitizr | 1 of 1 landed | 30 tasks, 73% landed |
| nucleosynthesis | 47 tasks, 9% landed (the local-model tasks) | 23 tasks, 35% landed |

- **Choose `direct`** for a small, well-specified task on a repository
  with strong checks, which today means the Forge kernel: there it
  landed 75% against `reviewed`'s 71%, and an operator who is around to
  answer a question makes the extra attention cheap. The record read
  here has no cost split by repository; the cost figures above are over
  all tasks.
- **Choose `reviewed`** for anything larger, for a repository with weak
  checks (nucleosynthesis, where `reviewed` itself lands only 35%), and
  for any task an operator will not be around to answer, since each
  question a `direct` task asks is a stall.
- **Task size.** The record read for this verdict carries no split of
  `direct`'s outcomes by files or lines changed, so "small" is a
  judgement and not a threshold: no cut-off is stated because none has
  been measured. The split is a query over the store, to be made before
  a number is put here.
- **Time to live.** `reviewed` lands in a median of 3271 s (p90 22809
  s). `direct` has one measured landing, so the two are not compared.

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

After each agent attempt, Forge fetches the explicit task branch into a
kernel-owned bare repository under `FORGE_HOME/repositories`, one per registered
repository. Its complete configuration is kernel-written, disables hooks, and
contains no executable configuration keys. This repository is never mounted
into a sandbox. Fetch is the only host Git operation that reads the agent's
Git metadata; Git's upload-pack does not honor repository-local hooks or pack
hooks. Before L0, Forge replaces the clone with a fresh checkout from that
repository, preserving ordinary files (including uncommitted changes) but
never the agent's Git metadata. Verification, hidden-suite overlays, and
attempt recording use this fresh tree. Only the selected branch and trusted
base are fetched into it, so other tasks' verification refs remain hidden.
Landing and push retain their existing flow.
