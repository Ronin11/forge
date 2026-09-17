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
- **`forge trace ID --json`** — everything about one task: its full
  record, every attempt's inputs/outputs/verdict, every kernel
  operation, and a diagnosis. One [`TraceDoc`](#tracedoc) object. Exits
  non-zero if the task does not exist.
- **`forge journal ID --json`** — what ran earlier in the task's piece
  of work. A JSON array of [`JournalEntry`](#journalentry) objects
  (`{task, attempt, step, state, said, found, reason}`), oldest first.
- **`forge workflows --json`** — the workflows and actions a task can
  run, with declared metadata and measured outcomes. A JSON object
  `{workflows, actions, min_runs_for_known, lookback}`; shaped for an
  agent choosing a workflow, not documented field-by-field here since no
  client (`tui/`, `web/`) reads it today — treat its shape as informal
  until a client depends on it.
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

`forge doctor --json` also exists (a JSON array of
`{name, status, detail, hint}`) but no current client calls it; it is
listed for completeness, not as part of the stable contract.

The verb names above, as a plain fenced list a test can parse without
scraping this prose (`tests/boundary.rs` reads this block and
asserts every verb a client source file invokes appears in it):

```text
snapshot log requests decisions trace journal workflows stats events retry doctor plugin ref project initiative
```

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
| `created` | string | Legacy key for `created_at`: a localtime string, kept for compatibility. |
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

### `StatsDoc`

The document `forge stats --json` prints: `{workflows, steps, journal,
no_journal, projects, by_role, tools}`. `tools` is present only with
`--tools` (an object keyed by step name); otherwise it is omitted.
`projects` is present only when `forge stats` is not itself scoped to
one project or initiative.

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

**`by_role`** — array of `StatsRoleRow`, one per (role, provider, model)
combination with at least one attempt, role being the attempt's step
(`code`, `review`, and so on): `role`, `provider`, `model`, `attempts`,
`succeeded`, `succeeded_share` (null when `attempts` is 0),
`mean_turns`, `mean_cost_usd`, `mean_secs`. For the `code` role only,
also `landed` (tasks with an attempt in this group that landed) and
`broke_base` (of those, how many broke a later task's base — the same
defect-escape signal as `StatsWorkflowRow::broke_base`, keyed by the
group's own tasks instead of by workflow) with `broke_base_share`
(`broke_base` divided by `landed`; null when `landed` is null or 0).
Also for the `code` role only, the same delayed-cost signals as
`StatsWorkflowRow` (see above), keyed by the group's own landed tasks
instead of by workflow: `repair_cost_usd`, `true_cost_per_landed_usd`
(null when `landed` is null or 0), and `churn_share` (null when nothing
was added yet to measure). Outside the `code` role, `landed`,
`broke_base`, `broke_base_share`, `repair_cost_usd`,
`true_cost_per_landed_usd`, and `churn_share` are all omitted from the
JSON row entirely, since landing is not a role-specific concept. `forge
stats --by-role` is the text-mode view of the same rows.

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
- **`forge-web`** (`web/src/main.rs`, `web/src/index.html`, `web/src/app.js`): every
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
  `/api/projects` → `project list --json` for the `/projects` page;
  `/api/projects/<name>` → `project show <name> --json`,
  `/api/projects/<name>/initiatives` → `initiative list <name> --json`,
  and `/api/projects/<name>/backlog` → `project backlog <name> --json`,
  together for the `/projects/<name>` page; `/api/initiatives/<id>` →
  `initiative report <id> --json` for the `/initiatives/<id>` page,
  which is the outcome, the tasks and their states, and the rest of the
  generated report all from that one document.
