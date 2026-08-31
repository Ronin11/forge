# Forge plugins (M7)

Integrations — bars, notifiers, GitHub, Jira — live outside the core. The core
knows a manifest, a process, a token with scopes, a journal stream, and MCP
aggregation (`DESIGN.md` §17). This document is the plugin contract and the
specification of the first plugins.

## Plugin contract

A plugin is a directory — first-party under `plugins/<name>/` in the repo
(installable with `forge plugin install <name>`), third-party under
`~/.forge/plugins/<name>/` — whose base name matches the manifest name, with
`plugin.toml`:

```toml
name = "status-file"
version = "0.1.0"
description = "…"
command = ["./forge-status-file"]                # argv relative to the plugin dir; any language
build = ["go", "build", "-o", "forge-status-file", "."]  # optional; run once at install, in the plugin dir
capabilities = ["events"]                         # events | tools | intake | annotate
scopes = ["events:read", "work:read", "usage:read"]
restart = "always"                                # always | on-failure | never (default on-failure)
```

`internal/plugin` is the one home of this contract (manifest shape, capability
and scope vocabularies, discovery); the daemon refuses invalid manifests.

### Capabilities

- **events** — the plugin consumes the journal stream:
  `GET /api/v1/journal?follow=1&since=<cursor>` (SSE; cursor = journal id),
  framed as `event: journal` + `id: <journal id>` + `data: <entry JSON>` lines.
  An entry is `{"id", "ts", "kind", "entity_type", "entity_id", "payload"}` —
  strings and small JSON, no Go dependency needed. Without `follow=1` the same
  endpoint returns a JSON array (the polling fallback for older daemons). The
  daemon persists each plugin's last acknowledged cursor
  (`POST /api/v1/plugins/<name>/ack` with `{"cursor": N}`) so a restarted
  plugin resumes with no gaps and no duplicates beyond the last ack.
- **tools** — the plugin is an MCP server on its stdio (newline-delimited
  JSON-RPC 2.0: `initialize`, `tools/list`, `tools/call`, `ping` — the framing
  of `internal/mcpserve`). `forge mcp` aggregates core tools with enabled
  plugins' tools, namespaced `<plugin>_<tool>`, surfaced through
  `/api/v1/tools` and still gated per mode by `--allowedTools` and by the
  plugin's scopes (`tools:provide`). A plugin tool call is a span like any
  other. Enabling or disabling a tools plugin takes a daemon restart to change
  the aggregated tool list.
- **intake** — the plugin creates Work via the API with `external_refs`; how it
  learns of work (poll, webhook) is its business.
- **annotate** — `external_refs` on Work/Target/Proposal:
  `[{plugin, kind, id, url, label}]`; the UI renders them as links generically.

### Core responsibilities (the complete list)

Discover and validate manifests; supervise processes (start on daemon start,
restart per `restart` with exponential backoff, stderr captured into
`~/.forge/logs/plugins/<name>.log` with `component=plugin.<name>`); mint a
per-plugin token carrying only the declared scopes, shown and approved at
`forge plugin enable` (a request outside scope is 403 and journaled); serve the
journal stream with cursors; aggregate MCP servers; store `external_refs`;
expose plugin health on the System page and in `forge doctor`.
`forge plugin list|install|uninstall|enable|disable|logs|status`.

A plugin's environment is `FORGE_SOCKET`, `FORGE_TOKEN`, `FORGE_PLUGIN_DIR`,
`FORGE_LOG_LEVEL`, `FORGE_LOG_FORMAT`, plus the pass-through list of
`DESIGN.md` §1.1 (`PATH`, `HOME`, … — without them a Python or Node plugin
cannot start) and nothing else from the daemon's environment.

### Scopes

`events:read`, `work:read`, `work:write`, `usage:read`, `kb:read`, `kb:write`,
`proposal:read`, `tools:provide`, `annotate:write`.

## First plugin: the Omarchy indicator

Two halves, split so the Forge half is not Omarchy-specific.

### Forge side — `plugins/status-file/`

Go, first-party, `events` capability. Subscribes to the journal and maintains
`~/.local/state/forge/status.json` (`XDG_STATE_HOME` respected) atomically
(tmp in the same directory + rename) on every relevant journal event
(`work.*`, `target.*`, `question.*`, `proposal.*`, `daemon.*`, `budget.*`,
debounced 300 ms so bursts coalesce into one write) and on a 5 s heartbeat:

```json
{ "schema": 1, "ts": "…", "daemon": "ok", "state": "idle|working|attention|throttled",
  "running": [{"work": "…", "title": "…", "routine": "inventory", "repo": "equitizr",
               "mode": "run", "elapsed_s": 143, "model": "sonnet",
               "url": "http://127.0.0.1:7340/tasks/…"}],
  "queued": 3, "blocked": 1, "deferred": 2,
  "human_queue": {"questions": 1, "proposals": 2, "verifications": 0,
                  "items": [{"kind": "question", "id": "…", "title": "…", "age_s": 320, "url": "…"}]},
  "usage": {"five_hour": {"utilization": 0.41, "resets_in_s": 5400, "target": 0.9},
            "seven_day": {"utilization": 0.22, "resets_in_s": 300000, "target": 0.9}},
  "last_failure": {"work": "…", "routine": "touch", "reason": "nonzero_exit", "ts": "…", "url": "…"},
  "ui": "http://127.0.0.1:7340" }
```

Notes on what is actually filled:

- `phase` and `tokens` on running items are omitted: the daemon does not
  persist heartbeat phase or in-flight token counts, and the plugin never
  computes locally what the API does not serve. `elapsed_s` comes from the
  target's `started_at`; `mode`/`model` from the latest attempt.
- `verifications` is 0 until `/api/v1/attention` serves verifications.
- `usage` is present only when the budget policy is active
  (`GET /api/v1/usage` answers 501 otherwise); a hard stop still surfaces
  through the queue's `hard_stop:<window>` deferred reason.
- `last_failure` covers failures seen in the journal within the last hour; on
  a fresh start the plugin scans the journal tail, so failures older than its
  start may be missing until events arrive — accepted.

`state` is computed in exactly one function (`computeState`): `throttled` if a
budget hard stop is active (either window's utilization at or past its
configured hard stop, or a queue entry deferred with reason `hard_stop:*`);
else `attention` if the human queue is non-empty or anything failed in the
last hour; else `working` if any attempt is running; else `idle`. Any bar or
status line can consume this file. If the daemon is down the file simply goes
stale; consumers treat `ts` older than 30 s as "daemon down".

### Omarchy side — `plugins/omarchy-indicator/`

A Quickshell bar widget + panel installed by
`forge plugin install omarchy-indicator` into
`~/.config/omarchy/plugins/ronin.forge/`; it only reads the status file. Icon
color follows `state` (idle = dim, working = accent, attention = urgent,
throttled = warning; stale = dim/off), a badge carries the human-queue total,
and the click panel shows running work, the human queue, queue counts, usage
meters, and the last failure. `forge plugin uninstall omarchy-indicator`
reverses the install, including the `shell.json` edit.

## Example third-party plugin: `plugins/examples/echo-tools/`

A committed reference copy of the throwaway MCP plugin the M7 smoke uses
(`SMOKE.md` §M7 step 4): copy it to `~/.forge/plugins/echo-tools`, enable it,
and its `ping` tool surfaces as `echo-tools_ping`. See its README.

## `github-issues` — GitHub issues as Forge tasks

A first-party `intake` + `annotate` plugin (Go, `plugins/github-issues/`).
GitHub is the inbox, Forge is the worker: it polls configured repositories for
open issues and, per new issue, creates a Forge task — an implement-mode task
by default — linked back to the issue via `external_refs`; then it watches
those tasks and comments the outcome on the issue when they finish.

It runs the host's `gh` CLI for every GitHub call and never handles a token
itself — `gh` authenticates through the passed-through XDG environment. Forge
calls go over `FORGE_SOCKET` with `FORGE_TOKEN`.

### Config — `<FORGE_PLUGIN_DIR>/github-issues.toml`

```toml
poll_seconds = 60          # default 60, minimum 15
[[repo]]
github    = "Ronin11/forge"   # owner/name; the GitHub repository
forge     = "forge"           # the registered Forge repository name
label     = "forge"           # only issues with this label; "" = every open issue
mode      = "implement"       # default "implement"
class     = "normal"          # default "normal"
autonomy  = "auto"            # default "auto"
integrate = false             # default false
comment   = true              # default true — comment on the issue on pickup + finish
done_label = "forge:done"     # optional; added when the task finishes (best-effort)
```

Each `[[repo]]` needs `github` (owner/name) and `forge`; anything else takes
its default. An absent config file makes the plugin idle (it logs
`no github-issues.toml; nothing to poll` and heartbeats) rather than crash.
Ingest and comment state is kept in `<FORGE_PLUGIN_DIR>/state.json` (atomic
write, 0600), keyed `"<github>#<number>"`, so restarts neither re-create tasks
nor re-comment outcomes.

### The intake → implement → comment loop

Every `poll_seconds` (and once on start):

1. **Ingest.** For each repo, `gh issue list` the open issues (filtered to
   `label`). For each issue not already tracked, `POST /api/v1/tasks` with the
   prompt built from the issue (the issue body is the acceptance criteria),
   `POST …/external-refs` a `{kind:"issue", id:"<github>#<n>", …}` ref, and —
   when `comment` — post a pickup comment. Only after the task is created is
   the issue recorded, so a create failure retries next cycle and a later
   best-effort step never duplicates the task.
2. **Reconcile.** For each ingested task, `GET /api/v1/tasks/<id>`; when it
   reaches a terminal state (succeeded, failed, unverified, partial,
   cancelled, merged, conflict) it comments the outcome (with the fix branch
   and the task URL), best-effort adds `done_label`, and marks the entry done
   so it never comments again.

Opening a PR from the fix branch is a natural future option; this plugin only
comments the outcome and does not open PRs.
