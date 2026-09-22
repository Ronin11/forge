# The client contract

`forge-tui` (`tui/`) and `forge-web` (`web/`) are clients of the `forge`
CLI and nothing else: neither opens the database or links the kernel
(`tests/boundary.rs` enforces that for the TUI). Everything a client
knows, it knows because it ran a `forge` verb and parsed its `--json`
output, or because it is reading `events.jsonl` through `forge events`.
This document is what a client may rely on: the verbs, the shape of
each JSON document, the event types, and the snapshot-then-subscribe
protocol that keeps a client's state in sync without polling.

A third client only has to follow this document, not read the engine,
to stay correct.

## Verbs

Every verb below is invoked as `forge <name> [args] --json` (or, for
`events`, with no `--json` flag — its output is always JSON lines) and
its stdout is exactly one JSON value, pretty-printed except where noted.
A non-zero exit means the stderr text is the error; a client shows it
and does not parse stdout.

- **`forge snapshot`** — no arguments. The whole state at one instant,
  plus the point in the event log to subscribe from. See
  [Snapshot](#snapshot-document).
- **`forge log --json [--limit N] [--state S] [--repo P] [--before ID]
  [--grep TEXT] [--workflow W] [--project NAME] [--initiative ID]`** —
  tasks, newest first. A JSON array of [`TaskRow`](#taskrow). `--limit`
  defaults to 20; `--before` pages backward by id; `--grep` matches the
  task text or an exact id; `--state` is one of `queued`, `running`,
  `succeeded`, `failed`, `blocked`, `unverified`, `withdrawn`.
- **`forge requests --json [--repo P]`** — blocked tasks and what each
  is waiting on. A JSON array of [`RequestRow`](#requestrow).
- **`forge decisions --json [--repo P]`** — operator and supervisor
  answers, newest first. A JSON array of [`DecisionRow`](#decisionrow).
- **`forge ref list ID --json`** — external references recorded on one
  task: the pull request it landed as, the issue it came from. A JSON
  array of [`RefRow`](#refrow). They are also carried on `TraceDoc.task`
  (see below).
- **`forge ref add ID --kind K --url U [--label TEXT] [--by NAME]`** —
  record a reference on a task. Not `--json`; a client re-reads
  `forge ref list` or `forge trace` for the row it just created.
- **`forge project list --json`** — every project, alphabetically. A JSON
  array of [`ProjectRow`](#projectrow).
- **`forge project show NAME --json`** — one project. A single
  [`ProjectRow`](#projectrow) object. Exits non-zero if `NAME` names no
  known project.
- **`forge project backlog NAME --json`** — one project's backlog, oldest
  first. A JSON array of [`BacklogRow`](#backlogrow). `--add`/`--done`
  write before printing, so a client re-reads this rather than parsing
  the write's own (non-JSON) output.
- **`forge project view NAME --json`** — everything the customer portal's
  page needs for one project, in the customer's own words (see
  docs/PORTAL.md, "What they see"). A single [`PortalDoc`](#portaldoc)
  object. Exits non-zero if `NAME` names no known project.
- **`forge project portal NAME [--revoke]`** — mint a fresh customer
  portal token for a project and print its link path, `/p/<token>` (see
  docs/PORTAL.md, "What it is"). Not `--json`; there is nothing to parse
  beyond the printed path. `--revoke` first revokes every token minted
  earlier for this project, so only the fresh one keeps working; without
  it, an earlier link stays valid alongside the new one.
- **`forge project resolve-token TOKEN --json`** — the project a customer
  portal token opens (see docs/PORTAL.md, "What it is"): how
  `forge-portal` turns `/p/<token>` into the name it then passes to
  `forge project view`. Prints `{"project": NAME}`; exits non-zero if the
  token is unknown or revoked.
- **`forge initiative list [<project>] --json`** — every initiative, or
  only `<project>`'s, oldest first. A JSON array of
  [`InitiativeRow`](#initiativerow).
- **`forge initiative show ID --json`** — one initiative: its state, task
  counts, cost and settings. A single [`InitiativeRow`](#initiativerow)
  object. Exits non-zero if `ID` names no known initiative.
- **`forge initiative report ID --json`** — the generated report: the
  outcome, each task and how it ended, what verification refused, what
  the supervisor ruled, what reached the operator, cost and elapsed time.
  A single [`InitiativeDoc`](#initiativedoc) object.
- **`forge job list [<project>] --json`** — jobs (runs of a `kind = "run"`
  workflow, see docs/JOBS.md), newest first, or only `<project>`'s. A
  JSON array of [`JobRow`](#jobrow).
- **`forge job show ID --json`** — one job, with every step and effect it
  recorded. A single [`JobDoc`](#jobdoc) object. Exits non-zero if `ID`
  names no known job.
- **`forge job log <project> --json`** — a project's job effects across
  every one of its jobs, newest first. A JSON array of
  [`JobEffectRow`](#jobdoc).
- **`forge job fire <project> --webhook NAME [--input FILE] [--ref KEY]
  --token TOKEN`** — fire a project's webhook trigger (see docs/JOBS.md,
  "Triggers"): queue a job for the run workflow whose `[trigger]` is
  `on = "webhook"` with this `name`. Prints the job's id and nothing
  else; not `--json`. `FILE` is the delivery's body, a JSON object
  (default `{}`). `KEY` names the delivery: firing the same key again
  starts no second job and prints the first one's id, with exit 0 (a
  note on stderr says so); without it the key is the SHA-256 of the
  file. A non-zero exit is refused, with these stderr texts to tell the
  cases apart: `invalid webhook token` (missing, unknown, revoked, or
  another hook's; nothing else about the project or hook is said),
  `no run workflow` (no workflow claims that hook name),
  `per_day limit` (the workflow's cap), and anything about the input
  being JSON. `forge project webhook token|revoke|list` manage the
  tokens and are the operator's, not a client's: a client only ever
  holds a token it was handed.
- **`forge trace ID --json`** — everything about one task: its full
  record, every attempt's inputs/outputs/verdict, every kernel
  operation, and a diagnosis. One [`TraceDoc`](#tracedoc) object. Exits
  non-zero if the task does not exist.
- **`forge journal ID --json`** — what ran earlier in the task's piece
  of work. A JSON array of [`JournalEntry`](#journalentry) objects
  (`{task, attempt, step, state, said, found, reason}`), oldest first.
- **`forge graph REPO --json`** — the module graph as data (docs/LATER.md,
  "The code visualiser"): every source file `forge-repomap` extracts as a
  node, one module node per directory grouping files directly under it,
  and the import edges between files. `REPO` is a path to a repository's
  working tree, read directly; there is no task and no store behind this
  one. A single [`GraphDoc`](#graphdoc) object. The built-in `repo-graph`
  operation (docs/ACTIONS.md) writes this same document to
  `$FORGE_CACHE_DIR/graph.json` on every attempt, so a landing always
  leaves a fresh one behind.
- **`forge workflows --json`** — the workflows and actions a task can
  run, with declared metadata and measured outcomes. A JSON object
  `{workflows, actions, min_runs_for_known, lookback}`; shaped for an
  agent choosing a workflow, not documented field-by-field here since no
  client (`tui/`, `web/`) reads it today — treat its shape as informal
  until a client depends on it.
- **`forge workflows show NAME --json`** — one workflow in full, for the
  operator's own workflow page: the file's exact text, `source`
  (`"catalog"`, the operator's own `<FORGE2_HOME>/workflows/`, or `"repo"`,
  a project's own `.forge/workflows/` at its latest landed commit, found
  with `--project P` when the catalog has no workflow of that name), its
  `kind` (`"build"` or `"run"`), `path`, every resolved `steps[]` entry
  (`{name, kind, contract, model, max_turns, timeout_secs, description}`,
  the action each step actually runs, in order — a `kind = "run"`
  workflow's steps resolve the way `forge job start` resolves them,
  splicing in any sibling run workflow), and `measured`, the same
  per-workflow profile object `forge workflows --json`'s own
  `workflows[].measured` carries (`current`/`previous`/`all_versions`
  profiles — verified rate with its 95% interval, cost per verified
  success, run count — `regressed`, and `by_provider`); see
  [`WorkflowShowDoc`](#workflowshowdoc). Exits non-zero if `NAME` names no
  known workflow.
- **`forge workflows lint --stdin [--name NAME]`** — validate a candidate
  workflow file's text against the catalog, so an editor can lint as the
  operator types: reads the candidate from stdin, parses it the same way a
  real file would be, and resolves it against the operator's own catalog
  (every action or workflow reference it names, the data-flow rule,
  `[trigger]` for a `kind = "run"` file). `NAME` is the file name the
  candidate would be saved under (its own `name` must match, exactly as a
  real file's stem must); omitted, the candidate's own declared name
  stands in, so a fresh draft lints clean before the operator has chosen
  where to save it. Prints `{"problems": [{"line", "message"}, ...]}`,
  **every** problem found, not just the first — a candidate naming two
  unknown actions gets two entries, each on its own line; `line` is the
  1-based line: a syntax or shape error's own span, the line of the
  offending `action = "…"`/`workflow = "…"` for an unknown reference, or
  the `steps` key's line for a whole-flow problem (a data-flow violation)
  with no span of its own. Empty and exit 0 when the candidate is clean,
  non-empty and **exit 1** otherwise — the same "print everything, one
  exit code" shape as `forge workflows validate` for a repository's files.
  Reads the operator's catalog (`FORGE2_HOME`) to resolve against, unlike
  `forge workflows validate`, which needs neither — a candidate is checked
  against the live catalog it would join, not a bare parse — but **writes
  nothing**, not even to a fresh, not-yet-initialized home: linting never
  has the side effect `forge workflows` and every other catalog command
  have of writing the built-in workflows and actions into `FORGE2_HOME`.
- **`forge workflows put NAME --stdin --message TEXT [--repo PATH]`** —
  write verb: writes a candidate workflow file into the operator's
  catalog (`<FORGE2_HOME>/workflows/NAME.toml`) once it lints clean (the
  same checks `forge workflows lint --stdin` runs, against `NAME`), then
  commits just that file in the catalog's own git — already a repository,
  the same one `forge workflows` itself creates on first use — with
  `--message`, and prints the new commit hash. Refuses, writing nothing,
  on: a candidate that fails lint (its problems are printed, one per
  line, `NAME.toml:LINE: MESSAGE` or `NAME.toml: MESSAGE` with no line);
  a `NAME` that does not match the candidate's own declared `name`; or an
  empty `--message`. `--repo PATH` files a direct task on that
  repository's project instead of touching the catalog: the task adds or
  replaces `.forge/workflows/NAME.toml` with the candidate's exact
  content, so a repository's own automation still lands through the
  normal build-and-verify path rather than a direct write; stdout is the
  new task's id instead of a hash. Not `--json`.
- **`forge stats --json [--tools] [--step S] [--quality] [--journal] [--by-role]`** —
  outcomes per workflow version and per step. One
  [`StatsDoc`](#statsdoc) object. `--quality` (text mode only; the JSON
  form always carries the fields) prints defect escape per workflow
  instead: of the tasks that landed, how many broke the next task's
  base or were later repaired. `--journal` (also text mode only; the
  JSON form always carries `journal`/`no_journal`) prints the journal
  control arm's retrospective split instead: code attempts after the
  first, by whether they were handed a journal. `--by-role` (also text
  mode only; the JSON form always carries `by_role`) prints the runner
  breakdown instead: attempts, outcomes, cost and wall time per (role,
  provider, model).
- **`forge plugin list --json`** — every plugin found under
  `<FORGE2_HOME>/plugins` and the operator's `plugin_dirs`, where it came
  from, and whether it is enabled. A JSON array of
  [`PluginRow`](#pluginrow).
- **`forge plugin status [<name>] --json`** — whether a plugin (or, with
  no name, every plugin) is enabled. A JSON array of
  [`PluginStatusRow`](#pluginstatusrow), or a single such object when
  `<name>` is given. Exits non-zero if `<name>` names no known plugin.
- **`forge events [--since OFFSET] [--follow] [--task ID]`** — the event
  log as JSON lines, one [`Event`](#events) per line. See
  [Snapshot, then subscribe](#snapshot-then-subscribe).
- **`forge retry ID [--chain]`** — the one write verb a client uses (the
  TUI's `r`/`R`, the web UI's retry button, `POST /api/retry/<id>` on
  the web server). Not `--json`; a client shows its text output and then
  re-reads the lists itself, since `forge retry`'s own output is not
  meant to be parsed.
- **`forge answer ID TEXT [--by NAME]`** — write verb: answers the
  question task `ID` is blocked on and re-queues it as a retry, `--by`
  naming the contact when the answer came through a channel (the portal
  passes its contact name). Stdout is the new task's id; a non-zero exit
  is the error on stderr. Not JSON.
- **`forge ask PROJECT MESSAGE [--from NAME]`** — write verb: the front
  door (docs/INTAKE.md), sorting a customer message into a request, a
  question, a need or unclear and acting on it. Stdout is the one-line
  reply to show the customer. Not JSON.
- **`forge message record PROJECT --channel NAME (--from NAME | --to NAME)
  --text TEXT [--task ID]`** — write verb: records one message on a
  channel, inbound from a contact (`--from`) or outbound to one (`--to`),
  so a rule can later ask "has this contact replied since" (e.g. a
  `[skip_if]` command reading `forge message list --json`, docs/JOBS.md).
  `--task` links it to the task it was about, when there is one. Not
  `--json`; a client re-reads `forge message list` for the row it just
  created.
- **`forge message list PROJECT [--contact NAME] [--since UNIX]
  [--direction in|out] --json`** — a project's recorded messages, newest
  first. A JSON array of [`MessageRow`](#messagerow).

`forge doctor --json` also exists (a JSON array of
`{name, status, detail, hint}`) but no current client calls it; it is
listed for completeness, not as part of the stable contract.

The verb names above, as a plain fenced list a test can parse without
scraping this prose (`tests/boundary.rs` reads this block and
asserts every verb a client source file invokes appears in it):

```text
snapshot log requests decisions trace journal graph workflows stats events retry doctor plugin ref project initiative job deploy answer ask message
```

## Time

Time is UTC everywhere in the kernel and the record; a time zone is a
rendering concern of each client. Every timestamp field in every JSON
document below (`created_at`, `finished_at`, `started_at`, `due_at`,
`landed_at`, `settled_at`, `ts`, …) is an integer count of Unix seconds,
UTC. A client that shows a time to a person converts it to its viewer's
zone itself. The one legacy string, `TaskRow.created`, is also UTC. The
CLI's own text output prints every wall-clock time as UTC with a `Z`
suffix (`2026-09-21T07:00:00Z`), never in the process's zone. A workflow's
schedule trigger is evaluated in UTC too (docs/WORKFLOWS.md).

## Naming: unified vs. legacy keys

Several documents grew a JSON form before they had a consistent naming
scheme, so some rows carry two names for the same value: an original,
sometimes terse or inconsistent key, and a later unified one that
matches the vocabulary used everywhere else (`text` for the human-
readable line, `created_at` for a Unix-seconds timestamp, `question` for
what a blocked task is asking). Both are always present and always
agree; a client should read the **unified name** and may ignore the
older key. The older keys are never removed within a row's lifetime —
they exist because a client may already depend on them — but they are
not where new fields get added.

`StatsDoc`'s `legacy` maps (`WF`, `HASH`, `TASKS`, `OK`, and so on) are the one
exception that is explicitly time-limited: they are kept for one
release only and a client should already be reading the named fields.

## Fixtures

Three of the documents above are checked in as literal examples, not just
prose: `tests/fixtures/job.json` (`forge job show --json`),
`tests/fixtures/deploy.json` (`forge deploy log --json`), and
`tests/fixtures/portal.json` (`forge project view --json`, the customer
portal document). Each was captured once from real `forge` output on a
throwaway project (see `capture_job_deploy_and_portal_fixtures` in
`tests/e2e/fixtures.rs`, `#[ignore]`d so it only regenerates them when a
person deliberately reruns it and reviews the diff) and is parsed by
`forge-client`'s own `JobDoc`/`Deploy`/`PortalDoc` types in that same
file's other tests. A shape drift between what the CLI actually prints
and what those types expect fails a test, the same way `TaskRow`,
`Snapshot` and `TraceDoc` are checked against live `forge` output (not a
checked-in file) by `forge_client_parses_trace_snapshot_log_and_requests`
in `tests/e2e/listing.rs`.

## JSON documents

### `TaskRow`

One row of `forge log --json`, one task as the queue lists it.

| field | type | meaning |
|---|---|---|
| `id` | integer | Task id. |
| `state` | string | `queued`, `running`, `succeeded`, `failed`, `blocked`, `unverified`, or `withdrawn` (the operator decided not to do it; not a failure). |
| `workflow` | string | Workflow name the task ran (or will run). |
| `attempts` | integer | Attempts run so far. |
| `cost_usd` | number | Total cost across all attempts, in USD. |
| `repo` | string | Absolute path to the task's repository. |
| `text` | string | **Preferred.** The task's text, as given. |
| `task` | string | Legacy key for `text`; kept for compatibility. |
| `created_at` | integer | **Preferred.** Creation time, Unix seconds. |
| `created` | string | Legacy key for `created_at`: `YYYY-MM-DD HH:MM:SS`, UTC, kept for compatibility. |
| `finished_at` | integer or null | When the task reached a final state, Unix seconds; null while it is queued or running. |
| `project` | string or null | The project the task belongs to; null for a task predating projects that no migration could place. |
| `initiative` | integer or null | The initiative the task belongs to, if any. |

### `RequestRow`

One row of `forge requests --json`: a blocked task and what it is
waiting on.

| field | type | meaning |
|---|---|---|
| `id` | integer | Task id. |
| `kind` | string | `dependency`, `workflow`, `suite`, `question`, `review`, or `other` — what sort of thing is blocking it. |
| `to` | string or null | Who the question is addressed to (e.g. a Signal plugin contact's name, from the agent's `needs_input.to`); null means the operator. |
| `question` | string | **Preferred.** What the task is waiting on, in words. |
| `text` | string | Legacy key for `question`; kept for compatibility. |
| `tried` | string | What the blocking attempt tried before it stopped; empty if unknown. |
| `path` | string | For a `suite` request, the test file that contradicts the task; empty otherwise. |
| `workflow` | string | Workflow the task runs. |
| `repo` | string | Absolute path to the task's repository. |
| `task` | string | The task's text. |

### `DecisionRow`

One row of `forge decisions --json`: an operator's or the supervisor's
answer to a blocked task's question.

| field | type | meaning |
|---|---|---|
| `id` | integer | Decision id. |
| `task_id` | integer | The task the question came from. |
| `repo` | string | Absolute path to the repository. |
| `question` | string | The question that was answered. |
| `answer` | string | The answer's text. |
| `created_at` | integer | Unix seconds. |
| `answered_by` | string | `"operator"`, `"supervisor"`, or a channel plugin's contact name (see `forge answer --by`). |
| `citations` | string | Comma-separated: paths, `task N`, or `decision N`. |
| `retry_id` | integer or null | The task the answer re-queued, once known. |
| `outcome` | string or null | State of `retry_id`'s task (e.g. `succeeded`), or `null` until it is known to have landed, failed, or otherwise settled. |
| `answered_for` | string or null | Who the question was addressed to, copied from the task's `question_to` (see `RequestRow.to`) at answer time; null means the operator. |

### `RefRow`

One row of `forge ref list --json`, and of `TraceDoc.task.refs`: an
external reference a plugin or the operator recorded on a task.

| field | type | meaning |
|---|---|---|
| `id` | integer | Reference id. |
| `task_id` | integer | The task it was recorded on. |
| `kind` | string | Whatever the caller passed to `--kind`, e.g. `"pr"` or `"issue"`. Not a closed vocabulary. |
| `url` | string | The reference's URL. |
| `label` | string | Free text, e.g. the PR's title; empty if not given. |
| `by` | string | Who recorded it: `"operator"` by default, or a plugin's own name. |
| `created_at` | integer | Unix seconds. |

One `kind` does carry a convention: `repairs`, whose `url` is
`forge://task/<id>`, naming the earlier landed task this one fixes. It
is how a task says "this repairs task 41" without inventing a second
id space; `forge stats --quality` reads it to count a landed task as
repaired.

### `MessageRow`

One row of `forge message list --json`: a message recorded on a channel,
inbound from a contact or outbound to one.

| field | type | meaning |
|---|---|---|
| `id` | integer | Message id. |
| `project` | string | The project the message is about. |
| `channel` | string | Whatever the caller passed to `--channel`, e.g. `"signal"`. Not a closed vocabulary. |
| `contact` | string | The contact's name: who it came from (inbound) or was sent to (outbound). |
| `direction` | string | `"in"` or `"out"`. |
| `text` | string | The message's text. |
| `at` | integer | Unix seconds. |
| `task_id` | integer or null | The task this message was about, when there is one (a concierge exchange, an intake interview's question); null otherwise. |

### `ProjectRow`

One row of `forge project list --json` / `forge project show --json`: a
project, the repositories it works in, task counts by state, cost, and
its own defaults. See docs/PROJECTS.md for the layer this belongs to;
initiatives are a later build-order step.

| field | type | meaning |
|---|---|---|
| `name` | string | The project's name, its primary key. |
| `purpose` | string | One paragraph saying what the project is for. |
| `created_at` | integer | Unix seconds. |
| `repos` | array of `{repo, scope}` | Repositories the project works in. `repo` is an absolute path; `scope` is the paths within it the project owns (a JSON-encoded array, as a string), or `null` for the whole repository. |
| `queued`, `running`, `succeeded`, `failed`, `unverified`, `blocked`, `withdrawn` | integer | Task counts by state, across the project's tasks. |
| `cost_usd` | number | Total cost across every attempt of every task in the project. |
| `jobs_today`, `jobs_ok`, `jobs_failed`, `jobs_needs_human` | integer | Jobs (`kind = "run"` workflow runs) started in the last rolling 24h, counted separately from the task counts above: `jobs_today` is every one of them, the rest are of those how many reached each terminal state (see docs/JOBS.md step 1d). |
| `workflow` | string or null | Default workflow for a task in this project, set by `forge project set --workflow`; `null` falls to "direct". |
| `per_task_usd`, `per_initiative_usd` | number or null | Default cost caps, set by `forge project set`; `null` falls to the operator's config (per-task) or means no cap (per-initiative). |
| `supervisor_model` | string or null | Default supervisor model, set by `forge project set --supervisor-model`; `null` falls to the operator's. |
| `supervisor_per_lineage` | integer or null | Default supervisor answers per lineage, set by `forge project set --supervisor-per-lineage`; `null` falls to the operator's. |
| `protected` | array of string | Extra protected paths, on top of each repository's own `forge.toml`, set by `forge project set --protected` (repeatable). |

### `BacklogRow`

One row of `forge project backlog NAME --json`: a thing worth doing that
is not yet queued (see docs/PROJECTS.md, "Backlog").

| field | type | meaning |
|---|---|---|
| `id` | integer | The item's id, used by `--done`. |
| `project` | string | The project it belongs to. |
| `text` | string | What it says, one line or a paragraph. |
| `created_at` | integer | Unix seconds. |
| `done_at` | integer or null | Unix seconds it was marked done, or `null` while open. |

### `PortalDoc`

The document `forge project view NAME --json` prints: everything the
customer portal's page needs for one project, in the customer's own
words (see docs/PORTAL.md, "What they see"). Deliberately thin: no task
ids beyond a question's own (needed to answer it in place), no branches,
no costs, no attempt data, no verdict rows — those belong to the
operator's page (`forge trace`, `forge stats`), never to this one.

| field | type | meaning |
|---|---|---|
| `project` | string | The project's name. |
| `purpose` | string | The project's one-paragraph purpose; for the operator's own tools — never rendered on the customer's page (see docs/PORTAL.md). |
| `deploy_targets` | array of `{name, where_it_runs, last_deployed_at, check_ok, look_ok, screenshot}` | "Running for you": each deploy target, `where_it_runs` its `host` arg, `last_deployed_at` Unix seconds of its most recent deploy (`null` if never deployed), `check_ok`/`look_ok` that deploy's verdicts (`null` if it never ran or never declared a smoke url), `screenshot` the last look's screenshot path, `null` when the smoke step never ran. |
| `run_workflows` | array of `{name, jobs}` | "Running for you", continued: every run workflow this project's jobs have used, most recently run first; `jobs` its last three, newest first, from the same job rows `forge job list` serves (see [`JobRow`](#jobrow)). |
| `run_workflows[].jobs[]` | `{started_at, state, reason}` | One job: `started_at` Unix seconds, `state` `"ok"`, `"failed"`, `"needs_human"`, or one of `JobState`'s other values for a job still in flight; `reason` a one-line cause cut from the first failing check's tail (path-like tokens stripped, 120 characters on a word boundary), set only when `state` is `"failed"` or `"needs_human"` and the verdict names one. |
| `initiatives` | array of `{outcome, state, pieces}` | "Being built": every open initiative (never settled), newest first, capped at ten (`initiatives_more` the rest); `state` one of `"in progress"` or `"waiting on you"` (an open question on one of its tasks — see `questions`); `pieces` how many tasks make up the initiative so far. |
| `initiatives_more` | integer | How many open initiatives past the ten in `initiatives`; 0 when nothing was cut. |
| `questions` | array of `{task_id, text, asked_at}` | "Needs you": every open question on the project's tasks, answerable with `forge answer task_id ...`; `asked_at` Unix seconds, when the task blocked on it. |
| `landed` | array of `{text, pieces, landed_at}` | "Done": one line per landed initiative (`text` its outcome, `pieces` how many tasks it took) and one line per landed task belonging to no initiative (`text` its title if it was filed in the customer's own words, else a line derived from the request's first sentence with any path-like token stripped and cut at 120 characters on a word boundary; `pieces` `null`), merged and sorted newest first, capped at ten (`landed_more` the rest). |
| `landed_more` | integer | How many landed lines past the ten in `landed`; 0 when nothing was cut. |
| `brief` | `{where_it_runs, workflows}` or `null` | "Your plan": the most recent confirmed intake brief, `workflows` one paragraph per workflow in the person's own words (see docs/INTAKE.md); `null` for a project with no intake behind it. |
| `backlog` | array of `{id, text, created_at}` | The rest of "Your plan": open backlog items, cut from the brief. |

### `InitiativeRow`

One row of `forge initiative list --json` / `forge initiative show
--json`: an initiative, its derived state, task counts by state, cost
and its own settings. See docs/PROJECTS.md, "Initiative".

| field | type | meaning |
|---|---|---|
| `id` | integer | The initiative's id. |
| `project` | string | The project it belongs to. |
| `outcome` | string | One sentence saying what is true when the initiative is done. |
| `state` | string | `open`, `held`, `done`, or `done with failures` (see docs/PROJECTS.md, "State"). |
| `held_rule` | string or null | While `state` is `held`: `"budget"`, or the L0 rule name whose repeated failure triggered the stop rule. |
| `queued`, `running`, `succeeded`, `failed`, `unverified`, `blocked`, `withdrawn` | integer | Task counts by state, across the initiative's tasks. |
| `cost_usd` | number | Total cost across every attempt of every task in the initiative. |
| `budget_usd` | number or null | This initiative's own cost cap; `null` falls to the project's `per_initiative_usd`. |
| `stop_after_same_rule` | integer | Hold the initiative after this many of its tasks fail in a row on the same L0 rule. |
| `created_at` | integer | Unix seconds. |
| `settled_at` | integer or null | When every task reached a terminal state and the initiative's own record closed; `null` while still open or held. |

### `InitiativeDoc`

The document `forge initiative report ID --json` prints: the generated
report (see docs/PROJECTS.md, "One notification and one report").

| field | type | meaning |
|---|---|---|
| `id`, `project`, `outcome`, `state`, `held_rule`, `budget_usd`, `stop_after_same_rule`, `cost_usd`, `created_at`, `settled_at` | | as [`InitiativeRow`](#initiativerow). |
| `tasks` | array of `{id, state, reason, score}` | Every task in the initiative and how it ended. `score` is the assess directive's 0-10 maintainability score for that task's own landing, or `null` if it never ran (see docs/ACTIONS.md, "Assessment"). |
| `refused` | array of `{rule, count}` | How many attempts of the initiative's tasks each verification rule refused, by name. |
| `rulings` | array of `{task_id, question, answer, citations}` | Decisions the supervisor made on the initiative's tasks. |
| `questions` | array of `{task_id, question, answer}` | Questions that reached the operator; `answer` is `null` while the task is still blocked. |
| `deployed` | array of `{task_id, target, sha, check_ok, rolled_back_to}` | Deploys the initiative's tasks triggered on landing (see docs/DEPLOY.md, "When a deploy runs"). `check_ok` is `null` while the deploy is still running; `rolled_back_to` is the previous passing commit, or `null`. |
| `elapsed_secs` | integer or null | Seconds from creation to the last task's `finished_at`; `null` if nothing has finished yet. |

### `JobRow`

One row of `forge job list --json`: a job, one run of a `kind = "run"`
workflow (see docs/JOBS.md, "The record"). No repository, no branch, no
landing — a job starts from a trigger and ends when its effects are
verified.

| field | type | meaning |
|---|---|---|
| `id` | integer | Job id. |
| `project` | string | The project it belongs to. |
| `workflow` | string | The run workflow's name. |
| `workflow_hash` | string | Content hash of the workflow file this job ran under. |
| `landed_sha` | string | The project's landed commit this job ran the workflow's automation files at; empty if the project has never landed anything. |
| `trigger_kind` | string | `manual`, `schedule`, `message`, `webhook`, or `event`. |
| `trigger_ref` | string | What one firing was for: a schedule's due slot as a unix second, a message's id, a webhook delivery's key (the caller's `--ref`, else a SHA-256 of the body), an event's byte offset in `events.jsonl`; empty for a manual trigger. |
| `state` | string | `queued`, `running`, `ok`, `failed`, `needs_human` (a blocked question addressed to the contact or the operator — see docs/JOBS.md, "The human rung"), or `dropped`. |
| `dry_run` | bool | True when effects were only recorded, not performed (e.g. `forge job test`'s fixture replay). |
| `started_at` | integer | Unix seconds. |
| `finished_at` | integer or null | Unix seconds; `null` while queued or running. |
| `cost_usd` | number or null | Total cost of the run; `null` until it finishes. |
| `verdict_json` | string | The assertions' verdict, raw JSON in the same shape as a task attempt's `verdict_json`; empty until the job finishes. |

### `JobDoc`

The document `forge job show ID --json` prints: one job with every step
and effect it recorded.

| field | type | meaning |
|---|---|---|
| `id`, `project`, `workflow`, `workflow_hash`, `landed_sha`, `trigger_kind`, `trigger_ref`, `state`, `dry_run`, `started_at`, `finished_at`, `cost_usd`, `verdict_json` | | as [`JobRow`](#jobrow). |
| `steps` | array of `{id, job_id, seq, action, kind, provider, model, cost_usd, started_at, finished_at, exit_code, output_ref, tail}` | Every step of the job's run, in order; an operation's `setup` check is the step with `seq` -1. `kind` is `operation` or `directive`; `provider`/`model` are set only for a directive step, `exit_code` and `tail` only for an operation step. `tail` is the last 20 lines of the operation's stdout and stderr, pass or fail (empty when it printed nothing). `output_ref` is the file holding the step's output: a directive's validated output, or everything an operation printed (up to the kept 16 KiB), under the job's input directory. `forge-web`'s own `/api/job/<id>` (not `forge job show` itself) adds one more field per step, `output`: `output_ref`'s file, read and parsed as JSON, when this process can do both; absent otherwise. |
| `effects` | array of `{id, job_id, seq, kind, target, summary, dry_run}` | Every effect a step performed on the world, in order. `seq` is the step that produced it; `kind` is the operation's declared effect kind (e.g. `message`, `row`); `target` is what it acted on; `summary` is a short human-readable description — what the portal shows per run. Same row shape as `forge job log --json`'s, whose rows span every job in a project instead of just this one, newest first. |

### `TraceDoc`

The document `forge trace ID --json` prints: everything about one task,
built once and shared by `forge trace`, `forge show`, and the `--json`
form so all three agree.

Top level: `{task, attempts, ops, resolved, diagnosis, deploys, assessment}`.

**`task`** — the task's full record. Selected fields (most are exactly
the store's column names):

| field | type | meaning |
|---|---|---|
| `id`, `repo`, `text`, `state`, `reason` | | as elsewhere; `text` here has always been the task's text (there is no legacy `task` key inside `TraceDoc.task`). |
| `workflow`, `workflow_hash`, `workflow_text` | string | workflow name, its content hash, and its exact text at resolution. |
| `base_branch`, `base_sha`, `branch`, `worktree` | string | where the task's clone lives and what it branched from. |
| `model`, `max_turns`, `max_attempts`, `timeout_secs` | | run limits. |
| `checks` | array of string | operator-declared acceptance commands. |
| `show_checks`, `allow_protected`, `land` | bool | run flags. |
| `after` | array of integer | task ids this one waits on. |
| `verify_base` | string | the `forge-verify` commit the task is judged by. |
| `retry_of` | integer or null | the task this one re-queues; also exposed as `parent`. |
| `parent` | integer or null | same value as `retry_of`. |
| `children` | array of integer | tasks that retry this one. |
| `root` | integer | the first task in this lineage. |
| `lineage` | array of `TraceLineage` | every task in the lineage: `{id, parent, state, reason, workflow, cost_usd}`. |
| `refs` | array of [`RefRow`](#refrow) | external references recorded on the task: the pull request it landed as, the issue it came from. |
| `journal_enabled`, `context_enabled`, `resume_on_failure` | bool | run flags. |
| `context` | string | what the last `context` operation printed. |
| `journal` | string or null | the prose journal of earlier attempts in this piece of work; `null` if empty. |
| `interface` | string | what the coder is told about the tests. |
| `plan` | string | what the last `plan` directive returned. |
| `pushed` | bool | whether the branch has been pushed. |
| `budget_usd` | number or null | per-task cost cap override. |
| `created_at`, `started_at`, `finished_at` | integer / integer or null | Unix seconds. |
| `project` | string or null | the project the task belongs to. |
| `initiative` | integer or null | the initiative the task belongs to, if any. |
| `inputs` | `TraceTaskShape` | task shape at intake (see docs/ECONOMIST.md, "Task shape"): what the economist must condition on before the task even runs, computed once at enqueue and never revisited. `{text_len, path_tokens, tdd, declared_checks, project}`: `text_len` the task's text length in characters; `path_tokens` how many of its whitespace-separated words look like a path (a request naming two paths counts two); `tdd` whether the resolved workflow writes hidden tests (a step whose action is `"tests"`, directly or through composition); `declared_checks` the repository's own `[checks]` count in `forge.toml` at that moment (0 for a task enqueued before this field existed, since backfilling it needs the repository's config as it stood at the time, which the record does not keep); `project` the same value as the top-level `project` field above, repeated here so the economist's inputs live in one place. |

**`attempts`** — array of `TraceAttempt`, one per attempt:
`attempt_no`, `step`, `step_seq`, `state`, `reason`, `started_at`,
`finished_at`, `agent_exit`, `timed_out`, `num_turns`, `tool_calls`,
`cost_usd`, `agent_ms`, `commits`, `files_changed`, `dirty`,
`start_sha`, `end_sha`, `log_path`, `tokens`
(`{input, output, cache_read, cache_creation}`), `rate_limits`
(`{five_hour, seven_day}`), and four raw-JSON fields carried through
unchanged from what the attempt itself produced: `inputs`, `outputs`,
`verdict`, `envelope`. A client that wants what a coder attempt's
`outputs` or `verdict` contain reads `src/audit.rs`'s `Outputs` type and
`checks::CheckResult`; the web UI's run view (`web/src/app.js`)
picks specific keys out of `inputs`/`outputs` (`model`, `summary`,
`changed_files`, `tools`, and so on) as an example of what's there, not
an exhaustive list — new keys can appear without notice, since these
are raw pass-throughs, not part of the row's own stable shape.

**`ops`** — array of `TraceOp`: `id`, `seq`, `name`, `kernel` (false for
a user-declared `--check`), `started_at`, `ms`, `ok`, `exit`, `detail`,
`attempt_id` (null for a task-level op such as the initial clone or
landing), `output`.

**`resolved`** — raw JSON: the workflow's resolved action versions
(`workflows::Resolved`), i.e. `{steps: [...], pins: [...]}`.

**`diagnosis`** — array of `{what, action}`: the kernel's own read of
why the task ended as it did, and what a human or a retry could try.

**`deploys`** — array of `Deploy`, this task's deploys newest first: the
on-landing targets it triggered when it landed (see docs/DEPLOY.md, "When
a deploy runs"), empty for a task that never landed one. Same shape as
`forge deploy log --json`'s rows: `id`, `project`, `target`, `sha`,
`started_at`, `finished_at`, `check_ok` (null while running),
`check_output`, `rolled_back_to` (the previous passing commit, or null),
`reason`.

**`assessment`** — `Assessment` or null: the assess directive's most
recent run against this task's own landing (see docs/ACTIONS.md,
"Assessment"), null when the workflow never opted in, the task never
landed, or the run failed. `{score, findings, model, provider, cost_usd,
created_at}` — `score` is 0 (worst) to 10 (best); `findings` is an array
of `{path, finding, severity}` (`severity` is `notable` or `concern`);
`model` and `provider` are what ran it; `cost_usd` is what it cost.

### `GraphDoc`

The document `forge graph REPO --json` prints: the module graph
(docs/LATER.md, "The code visualiser") for a repository at its working
tree, built deterministically from `forge-repomap edges` (docs/ACTIONS.md)
— no model, no task, no store.

| field | type | meaning |
|---|---|---|
| `nodes` | array of `{path, kind, symbols, lines}` | One entry per source file `forge-repomap` extracts, plus one entry per directory that groups files directly under it. `kind` is `"file"` or `"module"`. For a file, `symbols` is its declared symbol count and `lines` its line count; for a module, both are the sum over the files grouped under it. A root-level file joins no module. |
| `edges` | array of `{from, to}` | One entry per import that resolves to another file in the tree (Rust `use`/`mod`, TypeScript/JavaScript relative imports and `require`, Python `import`/`from ... import`, Go imports within the module path); an import that resolves outside the repository is never an edge. |

### `WorkflowShowDoc`

The document `forge workflows show NAME --json` prints: one workflow in
full.

| field | type | meaning |
|---|---|---|
| `name` | string | The workflow's name. |
| `source` | string | `"catalog"` (the operator's own `<FORGE2_HOME>/workflows/`) or `"repo"` (found via `--project`, in that project's own `.forge/workflows/` at its latest landed commit). |
| `path` | string | Where the file lives: an absolute path for `"catalog"`, a path relative to the repository's root for `"repo"`. |
| `kind` | string | `"build"` or `"run"`. |
| `text` | string | The workflow file's exact text. |
| `steps` | array of `{name, kind, contract, model, max_turns, timeout_secs, description}` | Every step, resolved to the exact action it runs, in order. `kind` is `"directive"` or `"operation"`; `model`, `max_turns`, `timeout_secs` are `null` when neither the step nor its action sets one. A `kind = "run"` workflow's steps are resolved the way `forge job start` resolves them (splicing in any sibling run workflow); a `kind = "build"` workflow's the way a task resolves them at creation. |
| `measured` | object | The same per-workflow profile object as one entry of `forge workflows --json`'s `workflows[].measured`: `{current, previous, all_versions, regressed, by_provider}`, each a [`Profile`](../src/profile.rs) — `known`, `n` (runs), `rate`/`rate_lo`/`rate_hi` (verified rate with its 95% interval), `cost_per_task`, `cost_per_success`, and so on; `regressed` is the same flag `forge doctor` warns on. Informal, like `Workflow::measured` above — read the named fields, since no client depends on the exact shape yet. |

### `StatsDoc`

The document `forge stats --json` prints: `{workflows, steps, journal,
no_journal, projects, jobs, by_role, assessment_correlation,
human_attention, human_attention_projects, time_to_live,
time_to_live_projects, tools}`.
`tools` is present only with `--tools` (an object keyed by step name);
otherwise it is omitted. `projects` and `jobs` are present only when
`forge stats` is not itself scoped to one project or initiative.

**`workflows`** — array of `StatsWorkflowRow`, one per workflow name +
definition hash: `workflow`, `hash`, `pieces` (task count),
`succeeded`, `failed`, `blocked`, `unverified`, `attempts`,
`mean_cost_usd`, `cost_per_success_usd` (null if nothing succeeded),
`landed`, `cost_per_landed_usd` (null if nothing landed). Plus every
header-named legacy key flattened onto the same object: `WF`, `HASH`,
`TASKS`, `OK`, `FAIL`, `BLK`, `UNV`, `ATT`, `COST`, `$/OK`, `LANDED`,
`$/LANDED` — kept for one release only; read the named fields instead.

Defect escape, the two signals docs/LATER.md calls out: `broke_base`
(landed tasks whose `landed_sha` became a later task's `base_sha`,
where that later task's first `code` attempt carries a failing L1
verdict row on the unmodified base) and `repaired` (landed tasks named
by a later task's `repairs` reference, see [`RefRow`](#refrow)).
`broke_base_share` and `repaired_share` divide each by `landed`; both
are null when nothing landed. Both counts and shares are always
present in the JSON form; `forge stats --quality` is the text-mode
view of the same numbers.

Delayed cost, docs/LATER.md's follow-up to defect escape: `repair_cost_usd`
(a line-overlap attribution, not a path one — for each later landing on the
same repository within 30 days, the fraction of its cost equal to the
lines it removed or rewrote that a landed task's own landing added,
divided by all the lines it removed or rewrote, summed across the
workflow's landed tasks; a later landing that rewrote none of a task's
lines charges it nothing, and one following on from two landed tasks
splits its cost between them by how many of each task's lines it
rewrote) and `churn_share` (of the lines the workflow's landed tasks
added, the share a later landing on the same repository removed or
rewrote within 30 days; null until something has been added to measure).
`true_cost_per_landed_usd` is `(mean_cost_usd + repair_cost_usd) /
landed` — landing cost and delayed cost together, null when nothing
landed. All three are always present in the JSON form (subject to the
landed-count null rule above); `forge stats
--quality` prints them beside the defect-escape columns.

**`steps`** — array of `StatsStepRow`, one per workflow + step:
`workflow`, `step`, `attempts`, `succeeded`, `agent_failed`,
`checks_failed`, `needs_input`, `mean_turns`, `mean_first_edit` (null
if nothing edited), `mean_secs`, `cost_usd`, `mean_input_tokens` (null
if nothing reported usage). Plus the legacy keys `WF`, `STEP`, `ATT`,
`OK`, `AGENTF`, `CHECKF`, `ASK`, `TURNS`, `EDIT@`, `SECS`, `COST`,
`TOKENS` — same one-release caveat.

**`journal`** and **`no_journal`** — the journal control arm's
retrospective split (docs/LATER.md, "The journal measurement was
ill-posed three times"): both are a single `StatsJournalRow` object
over code attempts after the first (`attempt_no > 1`, step `code`),
grouped by whether `inputs_json`'s `journal` field was present and
non-empty (`journal`) or not (`no_journal`). Fields: `attempts`,
`succeeded`, `succeeded_share` (null when `attempts` is 0),
`mean_turns`, `mean_first_edit` (null if nothing in the group edited),
`mean_cost_usd`. Both objects are always present, zeroed out when a
side has no matching attempts yet.

**`by_role`** — array of `StatsRoleRow`, one per (role, provider, model,
kind) combination with at least one row, role being a build task
attempt's step (`code`, `review`, and so on) or a job's directive
step's action, and `kind` (`"attempt"` or `"job_step"`) telling the two
apart — a directive step's provider, model and cost are counted here
too (docs/JOBS.md, "Steps"), never merged with a build task's attempts
of the same role name: `role`, `provider`, `model`, `kind`, `attempts`,
`succeeded`, `succeeded_share` (null when `attempts` is 0),
`mean_turns`, `mean_cost_usd`, `mean_secs`. A `"job_step"` row has no
turns and no success/failure of its own recorded, so `succeeded` and
`mean_turns` are always 0 there. For the `code` role's `"attempt"` rows
only, also `landed` (tasks with an attempt in this group that landed)
and `broke_base` (of those, how many broke a later task's base — the
same defect-escape signal as `StatsWorkflowRow::broke_base`, keyed by
the group's own tasks instead of by workflow) with `broke_base_share`
(`broke_base` divided by `landed`; null when `landed` is null or 0).
Also for the `code` role's `"attempt"` rows only, the same delayed-cost
signals as `StatsWorkflowRow` (see above), keyed by the group's own
landed tasks instead of by workflow: `repair_cost_usd`,
`true_cost_per_landed_usd` (null when `landed` is null or 0), and
`churn_share` (null when nothing was added yet to measure). Everywhere
else — every `"job_step"` row, and `"attempt"` rows outside the `code`
role — `landed`, `broke_base`, `broke_base_share`, `repair_cost_usd`,
`true_cost_per_landed_usd`, and `churn_share` are all omitted from the
JSON row entirely, since landing is not a role-specific concept. `forge
stats --by-role` is the text-mode view of the same rows, with `KIND` as
a column; `--tools`/`tools` stays attempts-only, since a job's directive
step runs with no tools to count.

**`assessment_correlation`** — array of `CorrelationRow`, judging the
assess directive's fast proxy against the delayed-cost measures it
stands in for (docs/ACTIONS.md, "Assessment"): one row per measure,
`{measure, rho, n}`. `measure` is `"churn"` (per landed task, its
`churn_share`-style ratio: churned lines over added lines) or
`"repair_cost"` (per landed task, its `repair_cost_usd`). `rho` is
Spearman's rank correlation between the task's assess score and the
measure, over this scope's landed tasks that carry both an assessment
and the measure (`null` when fewer than two tasks carry both, or
either side has no variance to rank); `n` is how many tasks that rests
on. Two rows are always present, in `churn` then `repair_cost` order.
`forge stats --quality` prints the same two numbers as a line under the
defect-escape table.

**`human_attention`** — array of `HumanAttentionRow`, one per workflow
name + definition hash present in scope: what a person had to do for
its landed work, since minutes cannot be measured. Four counts:
`operator_answers` (decisions on this workflow's tasks with
`answered_by` other than `"supervisor"`), `hand_landed` (this
workflow's tasks landed by a human's `forge land`, never the
supervisor's own accept-and-land), `withdrawals` (this workflow's
tasks left `withdrawn`), and `hand_commits` (commits not authored as
Forge, on the base branch, between this workflow's landings and the
ones before them). `events` is the four summed; `events_per_landed` is
`events` divided by `landed` (`null` when nothing landed). Operator
bookkeeping (`forge initiative set`, `forge project set`, `forge gc`,
budget raises, renames) is not among the four signals by construction,
so it never counts against landed work. Unlike
`workflows`, a workflow whose only tasks were withdrawn still gets a
row here, since a withdrawal is itself a human-attention signal.

**`human_attention_projects`** — array of `HumanAttentionProjectRow`,
the same shape and signals as `human_attention` but per project instead
of per workflow version; present under the same scoping rule as
`projects` (only when `forge stats` is not itself scoped to one project
or initiative).

**`time_to_live`** — array of `TimeToLiveRow`, one per workflow name +
definition hash present in scope: how long a request took to go live.
Per landed task, that is `landed_at - created_at`, or, where a deploy
row is tied to the task, that deploy's `finished_at - created_at`
instead. `n` is how many landed tasks this rests on; `median_secs` and
`p90_secs` are the median and 90th percentile of those durations, in
seconds (both `null` when `n` is 0).

**`time_to_live_projects`** — array of `TimeToLiveProjectRow`, the same
measure as `time_to_live` but per project instead of per workflow
version; present under the same scoping rule as `projects`.

`forge stats --quality` prints `human_attention`,
`human_attention_projects` (when present), `time_to_live` and
`time_to_live_projects` (when present) as four more tables, in that
order, after the defect-escape and assessment-correlation output.

**`jobs`** — array of `StatsJobsRow`, one per project with a job started
in the last rolling 24h: `project`, `today` (every job of theirs started
in the window), `ok`, `failed`, `needs_human` (of those, how many
reached that state) — counted separately from `projects`'s task rollup,
since a job is not a task (docs/JOBS.md step 1d). `forge stats` prints
the same numbers as a table under the projects one.

### `PluginRow`

One row of `forge plugin list --json`: a plugin as discovered.

| field | type | meaning |
|---|---|---|
| `name` | string | The plugin's name (its manifest name, which matches its directory's base name). |
| `description` | string | From its `plugin.toml`. |
| `dir` | string | Absolute path to the plugin's directory. |
| `source` | string | Absolute path to the root it was discovered under: `<FORGE2_HOME>/plugins`, or one of the operator's `plugin_dirs`. |
| `capabilities` | array of string | any combination of `events`, `intake`, `annotate`. |
| `restart` | string | `always`, `on-failure`, or `never`. |
| `enabled` | bool | Whether the operator has enabled it. |

### `PluginStatusRow`

One row of `forge plugin status [<name>] --json`: whether a plugin is
enabled and, per the supervisor's last record, its running state.

| field | type | meaning |
|---|---|---|
| `name` | string | The plugin's name. |
| `enabled` | bool | Whether the operator has enabled it. |
| `state` | string | `running`, `restarting`, or `stopped`. |
| `pid` | integer or null | Set when `state` is `running`. |
| `uptime_secs` | integer or null | Set when `state` is `running`. |
| `restart_count` | integer or null | Set when `state` is `restarting`. |
| `last_exit` | string or null | Set when `state` is `stopped` and the supervisor has run it before; `null` for a plugin no worker has ever supervised. |

### Snapshot document

The document `forge snapshot` prints:

```json
{
  "tasks": [ TaskRow, ... ],
  "requests": [ RequestRow, ... ],
  "worker": { "running": bool, "pid": integer, "exe": string, "stale_binary": bool },
  "events_offset": integer
}
```

`tasks` is the newest 200 tasks (as `forge log --json` would show with
no filter); `requests` is every blocked task (as `forge requests --json`
would show with no filter). `worker` is `{"running": false}` when no
worker pid file exists. `events_offset` is the byte length of
`events.jsonl` at the instant the snapshot was taken — see
[Snapshot, then subscribe](#snapshot-then-subscribe).

## Events

`forge events` streams `events.jsonl` lines, filtered by `--since`
(byte offset), `--follow` (keep the process alive and print new lines
as they're appended), and `--task` (only that task's events). Each line
is one JSON object: the fields of one `Event` variant, tagged by
`"type"` (snake_case of the Rust variant name, e.g. `task_started`,
`attempt_done`), plus two fields every event carries beyond what
`report::Event` itself defines:

- `text` — the same one-line human-readable summary the terminal
  printer would show, so a client never needs its own renderer for a
  quick feed.
- `ts` and `task` — added when the event is appended to the log (not
  present on the value `to_json` returns in isolation, but always
  present on a line read back from `events.jsonl`/`forge events`):
  `ts` is Unix seconds, `task` is the task id the event belongs to.

Every variant, with its own fields (beyond `type`/`text`/`ts`/`task`):

| type | fields | meaning |
|---|---|---|
| `task_started` | `worktree`, `branch`, `base_branch`, `base_sha`, `model`, `max_turns`, `max_attempts`, `timeout_secs`, `sandboxed` | A task's clone is ready and it is about to run. |
| `task_queued` | `workflow`, `retry_of` (integer or null) | A task entered the queue. |
| `attempt_started` | `n`, `of` | Attempt `n` of `of` for the current task. |
| `tool_call` | `name` | The agent called a tool. |
| `agent_done` | `exit` (integer or null), `turns`, `tools`, `ms`, `cost_usd` (number or null), `timed_out` | The agent process finished. |
| `git_counted` | `commits`, `files`, `dirty` | What the attempt's clone shows after the agent ran. |
| `check` | `level`, `name`, `ok`, `ms`, `tail` | One acceptance check's result; `tail` is its output tail, only meaningful when `ok` is false. |
| `attempt_done` | `state`, `reason` | The attempt reached a final state. |
| `pushed` | `remote`, `branch` | The branch was pushed. |
| `push_failed` | `error` | The push failed. |
| `push_skipped` | — | No remote configured. |
| `task_done` | `state`, `attempts`, `cost_usd`, `reason`, `branch`, `pushed`, `compare` (string or null) | The task reached a final state. (`remove_cmd` exists on the Rust side but is never serialized — `#[serde(skip)]` — so it never appears on the wire.) |
| `note` | `text` only | A free-text note (its `text` *is* its content, not a summary of something else). |
| `op` | `name`, `kernel`, `ok`, `ms`, `detail` | One kernel or user operation (clone, landing, a `--check` command) finished. |
| `deploy_started` | `project`, `target`, `sha` | A deploy of `project`/`target` began. |
| `deploy_finished` | `project`, `target`, `sha`, `ok`, `rolled_back_to` (string or null) | A deploy reached a verdict; `rolled_back_to` is the previous passing commit it fell back to when `ok` is false. |
| `project_created` | `project`, `person` | `forge intake accept` created `project` for the first time, on `person`'s confirmed brief (see docs/INTAKE.md); a plugin's cue to send them their customer portal link (see docs/PORTAL.md). |
| `job_started` | `project`, `workflow`, `job_id`, `dry_run` | A job began running its steps, either `forge job start --now` or the worker's claimed run. |
| `job_finished` | `project`, `workflow`, `job_id`, `state`, `cost_usd` | A job reached a final state: `ok`, `failed`, `needs_human` or `dropped`. |

### What to re-read on which event

An event only says *that* something changed, not the new value; a
client re-reads the affected document with the verb above.

- **The task list** (`forge log --json`) and, alongside it, the request
  list (`forge requests --json`): re-read on `task_queued`,
  `task_started`, `task_done`, `attempt_done`, or `pushed`. (The TUI
  also treats `attempt_started` and `op` as list-dirty, which is a
  superset of this — always safe, just more re-reads than strictly
  necessary. The minimum a client must handle is the five types above,
  matched by the web UI's list view.)
- **A task's detail** (`forge trace ID --json`, and `forge journal ID
  --json` if shown): re-read on `task_done`, `attempt_done`, or
  `deploy_finished`, and only when the event's `task` field matches the
  task currently open.
- **The run view** (the task inside its workflow — `forge trace ID
  --json`'s `ops` and `attempts`): re-read on `task_done`,
  `attempt_done`, or `op`, again only for the task currently open.
- **The jobs list** (`forge job list --json`): re-read on `job_started`
  or `job_finished`.
- **A job the page started and is watching** (the prompter,
  `/workflows/new`): `job_started` naming the project and workflow it
  just asked for is how it learns the id, with no request of its own;
  `job_finished` naming that id is what re-reads `/api/job/<id>` (see
  "The prompter" above).
- **A workflow's measured profile** (`forge workflows --json`'s
  `workflows[].measured`, and `forge workflows show NAME --json`'s
  `measured`): re-read on `task_done` (a build workflow's profile moves
  when a task under it lands) or `job_finished` (a run workflow's profile
  moves when a job under it finishes).

A client that only wants a live feed (a scrolling line per event) needs
no re-read logic at all: every event's `text` is already the line to
show, keyed by its `task` field.

## Snapshot, then subscribe

A client never polls. The protocol is:

1. Call `forge snapshot` once. Keep its `tasks`, `requests`, and
   `worker`; remember `events_offset`.
2. Start `forge events --since <events_offset> --follow` as a
   subordinate process (or, for `forge-web`, proxy it as an SSE
   stream — see `GET /api/events?since=`). Every line from here on is
   an event that happened *after* the snapshot was taken; nothing is
   missed and nothing is replayed twice, because `events_offset` is the
   exact byte length of the log at the instant the snapshot read it.
3. Apply each event as it arrives: append it to any live per-task feed,
   and re-read the documents named in
   [What to re-read on which event](#what-to-re-read-on-which-event).
4. If the subscription process ever needs restarting (reconnect after
   an error, a periodic full refresh as cheap insurance), take a fresh
   snapshot and restart the subscription from its new offset — the same
   two-step dance, never a bare re-subscribe with a guessed offset.

`events.jsonl` is bounded (rotated to `.jsonl.1`/`.jsonl.2` past 50MiB);
`forge events`, when it notices the file is now shorter than the
position it was reading from, starts over from the new file's
beginning. This only matters to a long-running `--follow` subscription
across a rotation, not to the snapshot protocol itself.

## What each client actually reads

- **`forge-tui`** (`tui/src/main.rs`): `snapshot` at startup and every
  60 seconds; `events --since <offset> --follow` for the live stream;
  `log --json --limit 60` and `requests --json` on a list-dirty event;
  `trace ID --json` to open a task and again on a trace-dirty event for
  the task currently open; `forge retry [--chain]` to act. The queue
  table's `TaskRow.project` is shown as a column; the task view shows
  `TraceDoc.task.initiative` alongside the rest of the record.
  Every time it shows (a job's `started_at`, `finished_at`, `due_at`) is Unix
  seconds turned into `2026-09-21 07:00` by `tui/src/time.rs` alone, in the
  terminal's local zone: the environment's (`TZ`, else the system's), read
  when drawn, and UTC when it names none. `tui/tests/snapshots.rs` pins a local
  and a UTC rendering under a fixed `TZ`.
- **`forge-web`** (`web/src/main.rs`, `web/src/index.html`, `web/src/app.js`, `web/src/time.js`): every
  route under `/api/` runs one verb and passes its JSON through
  untouched — `/api/snapshot` → `snapshot`, `/api/tasks` → `log --json`
  (query params map to `--limit`/`--before`/`--grep`/`--state`/
  `--workflow`/`--repo`/`--project`), `/api/requests` → `requests --json`,
  `/api/task/<id>` → `trace <id> --json`, `/api/journal/<id>` →
  `journal <id> --json`, `/api/events?since=` → `events --since
  --follow` reframed as one SSE `data:` line per event, and
  `POST /api/retry/<id>` → `forge retry <id>`. The browser's list view,
  detail view, and run view apply the same re-read rules as above; the
  list view's `TaskRow.initiative`, when set, links to `/initiatives/<id>`.
  `/api/plugins` runs `plugin list --json` and `plugin status --json`
  through `forge-client`'s typed `PluginRow`/`PluginStatusRow` and
  merges them by name for the `/plugins` page;
  `POST /api/plugins/<name>/enable` and `.../disable` → `forge plugin
  enable|disable <name>`; `/api/plugins/<name>/logs` → `forge plugin
  logs <name>` (no `--follow`), served as plain text.
  `/api/workflows` merges `forge-client`'s typed `workflow_list`: one call
  with no project for the operator's catalog, then one per project
  (`forge project list --json`)
  with `--project`, keeping only that call's `source: "repo"` entries and
  tagging each with the project it came from — the `/workflows` list page's
  rows. `/api/workflows/<name>[?project=P]` → `forge-client`'s typed
  `workflow_show` (`forge workflows show NAME [--project P] --json`), for
  the `/workflows/<name>` editor page's text, resolved steps, and measured
  profile. `POST /api/workflows/<name>/lint` — the body is the candidate
  text, plain, not JSON — runs `forge-client`'s `workflow_lint`
  (`forge workflows lint --stdin --name NAME`) on every debounced change
  and returns `{"problems": [...]}`, the same shape `forge workflows lint`
  prints. `POST /api/workflows/<name>` is the Save control: a JSON body
  `{"text", "message", "project"}`; `project: null` runs `forge-client`'s
  `workflow_put` with no `--repo`, committing straight into the operator's
  catalog (`{"result": "committed", "hash": ...}`); a `project` name
  resolves that project's first repo (`forge project show NAME --json`'s
  `repos[0].repo`) and runs `workflow_put` with `--repo`, filing a task
  instead (`{"result": "filed", "task_id": ...}`) — the editor labels this
  control "file as a task" rather than "save" when `source` is `"repo"`.
  **The prompter.** `POST /api/workflows/draft` is `/workflows/new`'s
  "Draft it": a JSON body `{"description"}`, written as `{"description":
  ...}` to a private temporary file and handed to `forge job start forge
  author-workflow --now --input <file>` (docs/WORKFLOWS.md, "Authoring"),
  blocking until the job ends the way the hooks route blocks on `forge job
  fire`. Success is `{"job": <id>}`; a blank description is **422** before
  any job starts, and any other failure to start is **502** with
  `{"error": "..."}`. `/api/job/<id>` (below) is then how the page reads
  what that job did. `/api/job/<id>` itself reads a step further than
  `forge job show ID --json`'s own JSON: for every step whose
  `output_ref` names a file this process can read and parse as JSON, that
  parsed value is attached to the step as `output` — best-effort, so a
  step with no readable or parseable `output_ref` simply has none. This is
  how the page reads the `draft-workflow` step's `{name, kind,
  description, toml, rationale, open_questions}` without a second
  command, and comes free to every other job step's own structured
  output. A job that ends `needs_human` has no `output` to read instead:
  the page re-reads `/api/tasks?project=P&state=blocked` once, for the
  newest row whose `task` is `"job question"` (the same task
  `job::ask` files — docs/JOBS.md, "The human rung"), then `/api/task/<id>`
  for its `reason`, the question text, and links to `/tasks/<id>`.
  Every time the page shows is Unix seconds from the server (`created_at`,
  `started_at`, `finished_at`, `due_at`, an event's `ts`), turned into text
  by `web/src/time.js` alone: `fmtTime` renders `2026-09-21 07:00` in the
  browser's own zone, `fmtSpan` and `fmtAgo` the relative forms (a plugin's
  uptime, a job's due time). `web/tests/time.rs` runs it under `node` in
  fixed zones.
  `/api/projects` → `project list --json` for the `/projects` page;
  `/api/projects/<name>` → `project show <name> --json`,
  `/api/projects/<name>/initiatives` → `initiative list <name> --json`,
  and `/api/projects/<name>/backlog` → `project backlog <name> --json`,
  together for the `/projects/<name>` page; `/api/initiatives/<id>` →
  `initiative report <id> --json` for the `/initiatives/<id>` page,
  which is the outcome, the tasks and their states, and the rest of the
  generated report all from that one document.
  **Webhooks.** `POST /hooks/<project>/<name>` is the one route not behind
  the web token: its credential is the hook's own token, sent as
  `Authorization: Bearer <token>` (never in the URL or a cookie), and the
  only reach it gives is `forge job fire`. The request body — a JSON
  object, at most 1 MiB — is written to a private temporary file and
  passed as `--input`; the delivery's key is `?ref=<key>` or an
  `Idempotency-Key` header, else none, so the kernel keys it on the body.
  `project` and `name` are letters, digits, `-`, `_` and `.`; anything
  else is a 404. The server never judges the token: it passes it on, and
  reads the kernel's refusal (above) back as a status —
  `invalid webhook token` and a missing header are **401**, `no run
  workflow` **404**, `per_day limit` **429**, a body that is not a JSON
  object (or a hook two workflows claim) **422**, an oversized body
  **413**, a method other than POST **405**, and any other failure
  **502**. Success is **200** with `{"job": <id>, "output": "<id>"}`,
  for a delivery already fired as well as for a new one: the sender's
  retry is answered like the first, and only one job exists. An error
  body is `{"error": "..."}` and never contains the token.
- **`forge-portal`** (`portal/src/main.rs`): `GET /p/<token>` runs
  `project resolve-token <token> --json` to find the project, then
  `project view <name> --json` for the page's four read-only sections —
  Running for you, Being built, Done, Your plan (Needs you and the Ask
  box are a later build-order step; see docs/PORTAL.md, "Build order").
  `GET /p/<token>/shot/<target>` streams that target's last-look
  screenshot file, whose path is `PortalDoc.deploy_targets[].screenshot`.
  An unknown or revoked token, a target with no screenshot, or any other
  route is a plain 404 page.
