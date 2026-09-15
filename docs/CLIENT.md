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
  [--grep TEXT] [--workflow W]`** — tasks, newest first. A JSON array of
  [`TaskRow`](#taskrow). `--limit` defaults to 20; `--before` pages
  backward by id; `--grep` matches the task text or an exact id;
  `--state` is one of `queued`, `running`, `succeeded`, `failed`,
  `blocked`, `unverified`.
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
- **`forge stats --json [--tools] [--step S] [--quality]`** — outcomes
  per workflow version and per step. One [`StatsDoc`](#statsdoc)
  object. `--quality` (text mode only; the JSON form always carries the
  fields) prints defect escape per workflow instead: of the tasks that
  landed, how many broke the next task's base or were later repaired.
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
snapshot log requests decisions trace journal workflows stats events retry doctor plugin ref
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
| `state` | string | `queued`, `running`, `succeeded`, `failed`, `blocked`, or `unverified`. |
| `workflow` | string | Workflow name the task ran (or will run). |
| `attempts` | integer | Attempts run so far. |
| `cost_usd` | number | Total cost across all attempts, in USD. |
| `repo` | string | Absolute path to the task's repository. |
| `text` | string | **Preferred.** The task's text, as given. |
| `task` | string | Legacy key for `text`; kept for compatibility. |
| `created_at` | integer | **Preferred.** Creation time, Unix seconds. |
| `created` | string | Legacy key for `created_at`: a localtime string, kept for compatibility. |
| `finished_at` | integer or null | When the task reached a final state, Unix seconds; null while it is queued or running. |

### `RequestRow`

One row of `forge requests --json`: a blocked task and what it is
waiting on.

| field | type | meaning |
|---|---|---|
| `id` | integer | Task id. |
| `kind` | string | `dependency`, `workflow`, `suite`, `question`, `review`, or `other` — what sort of thing is blocking it. |
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
| `answered_by` | string | `"operator"` or `"supervisor"`. |
| `citations` | string | Comma-separated: paths, `task N`, or `decision N`. |
| `retry_id` | integer or null | The task the answer re-queued, once known. |
| `outcome` | string or null | State of `retry_id`'s task (e.g. `succeeded`), or `null` until it is known to have landed, failed, or otherwise settled. |

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

### `TraceDoc`

The document `forge trace ID --json` prints: everything about one task,
built once and shared by `forge trace`, `forge show`, and the `--json`
form so all three agree.

Top level: `{task, attempts, ops, resolved, diagnosis}`.

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

### `StatsDoc`

The document `forge stats --json` prints: `{workflows, steps, tools}`.
`tools` is present only with `--tools` (an object keyed by step name);
otherwise it is omitted.

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

**`steps`** — array of `StatsStepRow`, one per workflow + step:
`workflow`, `step`, `attempts`, `succeeded`, `agent_failed`,
`checks_failed`, `needs_input`, `mean_turns`, `mean_first_edit` (null
if nothing edited), `mean_secs`, `cost_usd`, `mean_input_tokens` (null
if nothing reported usage). Plus the legacy keys `WF`, `STEP`, `ATT`,
`OK`, `AGENTF`, `CHECKF`, `ASK`, `TURNS`, `EDIT@`, `SECS`, `COST`,
`TOKENS` — same one-release caveat.

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
enabled. Supervision (running, pid, uptime, restarts, last exit) is not
implemented yet; `supervision` says so until it is.

| field | type | meaning |
|---|---|---|
| `name` | string | The plugin's name. |
| `enabled` | bool | Whether the operator has enabled it. |
| `supervision` | string | Always `"not yet implemented"` for now. |

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
  --json` if shown): re-read on `task_done` or `attempt_done`, and only
  when the event's `task` field matches the task currently open.
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
  the task currently open; `forge retry [--chain]` to act.
- **`forge-web`** (`web/src/main.rs`, `web/src/index.html`, `web/src/app.js`): every
  route under `/api/` runs one verb and passes its JSON through
  untouched — `/api/snapshot` → `snapshot`, `/api/tasks` → `log --json`
  (query params map to `--limit`/`--before`/`--grep`/`--state`/
  `--workflow`/`--repo`), `/api/requests` → `requests --json`,
  `/api/task/<id>` → `trace <id> --json`, `/api/journal/<id>` →
  `journal <id> --json`, `/api/events?since=` → `events --since
  --follow` reframed as one SSE `data:` line per event, and
  `POST /api/retry/<id>` → `forge retry <id>`. The browser's list view,
  detail view, and run view apply the same re-read rules as above.
