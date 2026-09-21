# Plugins

Integrations live outside the kernel: a notifier, an inbox that files
tasks, a bar widget, a bridge to an issue tracker. A plugin is a program
Forge starts and keeps running. It talks to Forge the way every client
does, through the CLI and its JSON (`docs/CLIENT.md`), so it can be
written in any language and owes nothing to Forge's build.

This document is the contract. It is deliberately smaller than Forge 1's:
what that system built as a socket, bearer tokens and a journal stream is
already the CLI's public surface here.

## What a plugin is

A directory whose base name matches its manifest name, holding
`plugin.toml`:

```toml
name = "notify"
description = "posts a desktop notification when a task needs a human"
run = ["./notify.sh"]                    # argv, resolved against the plugin dir
build = ["cargo", "build", "--release"]  # optional; run once at install
capabilities = ["events"]                # events | intake | annotate
restart = "on-failure"                   # always | on-failure | never
```

`run` is the program. `capabilities` declares what the plugin does, and
is a promise the operator reads, not a permission the kernel enforces
(see Trust). `restart` is the supervision policy, `on-failure` by
default. Unknown keys are refused, as in an action file.

## Where plugins live

`<FORGE2_HOME>/plugins/<name>/` first, then every directory listed under
`plugin_dirs` in the operator config:

```toml
# <FORGE2_HOME>/config.toml
plugin_dirs = ["~/.config/forge2/plugins", "/opt/forge2/plugins"]
```

`~` expands; a relative path resolves against the config file's own
directory, so what Forge discovers never depends on its working
directory. An earlier root wins a duplicate name, and the shadowed copy
is a warning. A configured root that does not exist is a warning, never
an error. The rules, the validation and the listing of problems are the
same ones the workflow directory has (`src/workflows.rs`), and for the
same reason: one broken file must not stop everything else from loading.

## Capabilities

**events.** The plugin follows what happened: `forge events --since
<offset> --follow` streams one JSON object per line, forever. The offset
is a byte offset into the log, and `forge snapshot` returns the offset
that matches the state it describes, so the honest start is a snapshot
then a subscription from its `events_offset`. Every event type and its
fields are in `docs/CLIENT.md`.

The plugin owns its cursor. Forge does not record one: a plugin that
wants to resume without gaps writes the last offset it handled into its
own state directory and starts there. This is the one place this
contract is smaller than Forge 1's on purpose. The kernel has no
business tracking a plugin's progress when the plugin can.

**intake.** The plugin creates work: `forge add <repo> <text>` with
whatever flags the request calls for. How it learns of the work, an
inbox, a webhook, a poll, is its business.

**annotate.** The plugin records references on a task: the pull request
it landed as, the issue it came from. `forge ref add <task> --kind
<kind> --url <url> [--label <text>] [--by <name>]` inserts one;
`forge ref list <task> [--json]` reads them back. `forge trace --json`
carries a task's references too, and `forge show` prints them under the
lineage as `ref` lines. As with `events` and `intake`, declaring
`annotate` is a promise the operator reads, not a permission the kernel
enforces: nothing stops a plugin that didn't declare it, or the
operator directly, from calling `forge ref add`, and nothing checks
that a plugin declaring it ever does. The kernel enforces nothing here
beyond the manifest itself being valid.

**message.** The plugin keeps the message record for its channel: what a
contact said and what was said back, so a rule can later ask "has this
contact replied since" (e.g. a `[skip_if]` command reading `forge
message list --json`, docs/JOBS.md). `forge message record <project>
--channel <name> (--from <contact> | --to <contact>) --text <text>
[--task <id>]` inserts one, `--from` for an inbound message, `--to` for
an outbound one; `forge message list <project> [--contact <name>]
[--since <unix>] [--direction in|out] --json` reads them back, newest
first. As with the other capabilities, declaring `message` is a promise
the operator reads, not a permission the kernel enforces.

`forge message record --from` is also the message trigger's trigger point:
recording an inbound message queues a job for every run workflow of the
project whose `[trigger] on = "message"` has a `contact` of `"*"` or this
contact, once per message, for the worker to run (docs/JOBS.md,
"Triggers"). A channel plugin needs nothing beyond that record call to
start automations; an outbound message (`--to`) starts none.

A plugin may declare more than one, and most useful ones do: watch for
a blocked task, ask a person, file the answer with `forge answer`.

**Not yet: tools.** Forge 1 aggregated plugin MCP servers into the tool
list the agent sees. Forge 2's agent is the claude CLI, which loads its
own MCP servers, so the shape here would be a generated per-task config
rather than an aggregator, and it is deferred because a plugin tool
with network access is a new path out of what the verification rules
can see, which deserves its own design.

## Trust

A plugin runs as the operator, with the operator's authority. It can run
`forge` and do anything the operator could: queue work, answer a
question, land a branch, read every repository Forge knows. There are no
scopes and no tokens.

This is the same posture as a workflow operation, which is already an
arbitrary shell command the operator chose to install. Install a plugin
the way you would install a shell script that runs on a timer as you.

Forge 1 had scoped tokens because it had a daemon with an HTTP API and a
socket to put them on. Forge 2 has neither: the CLI is the interface and
it runs as you. Scopes would mean building a mediated surface, and that
work belongs with multi-tenancy, not here.

## Supervision

The worker starts every enabled plugin when it starts and stops them when
it drains. A plugin is not a task: it has no attempts, no verdict, no
budget, and its failure never fails a task.

- **start**: `run` in the plugin directory, with the environment below.
- **restart**: `always` restarts it whenever it exits; `on-failure` only
  on a non-zero exit; `never` leaves it stopped. Backoff doubles from one
  second to a minute, and resets after the process has stayed up a
  minute.
- **output**: stdout and stderr to `<FORGE2_HOME>/logs/plugins/<name>.log`,
  which `forge plugin logs <name>` prints.
- **stop**: SIGTERM, then SIGKILL after ten seconds.

`forge doctor` reports each enabled plugin: running, restarting, or
stopped with its last exit.

## Environment

A plugin is given exactly what it needs to talk back, and nothing else
from the worker's environment beyond the pass-through list every agent
and check gets (`PATH`, `HOME`, and the rest in `src/agent.rs`).

| Variable | Meaning |
|---|---|
| `FORGE_BIN` | The forge binary to call. Always set; never assume `forge` is on `PATH`. |
| `FORGE2_HOME` | Forge's data directory, so a plugin's `forge` calls see the same store. |
| `FORGE_PLUGIN_DIR` | The plugin's own directory: its config, its assets. |
| `FORGE_PLUGIN_STATE` | A directory Forge creates for the plugin to keep its cursor and anything else it must remember across restarts. |

A plugin's own configuration is a file in its directory, read by the
plugin. Forge does not parse it and has no opinion about its shape.

## The verbs

```
forge plugin list                 every plugin found, where it came from, enabled or not
forge plugin status [<name>]      running state, pid, uptime, restarts, last exit
forge plugin enable <name>        enable and start it
forge plugin disable <name>       stop it and leave it installed
forge plugin install <path>       copy into <FORGE2_HOME>/plugins/<name> (refusing a name already
                                   installed there, validating the manifest first), then run `build`
forge plugin uninstall <name>     stop it, clear its enabled flag, remove the installed copy;
                                   its plugins-state is left alone
forge plugin logs <name> [-f]     its log
```

`list` and `status` also answer `--json`, in the shape `docs/CLIENT.md`
records, so the clients can show plugins without shelling out to
anything else.

## The plugins in this repository

Four plugins ship here, each `forge plugin install`-able straight from a
checkout, and each a different shape a plugin can take.

**notify** (`events`) is the reference plugin, and the one to copy. It is
a shell script: it takes a snapshot, subscribes from its offset, and
runs a command of the operator's choosing when a task blocks on a
question, fails, or lands, and when a deploy finishes (see
docs/DEPLOY.md, "When a deploy runs"). It keeps its cursor in
`FORGE_PLUGIN_STATE`, so a restart resumes where it stopped. Its
configuration is `plugins/notify/command`, a script `notify.sh` runs
with the task, its state and its reason as arguments for a `task_done`
event, or `deploy`, the project, target, sha and status for a
`deploy_finished` one; `command.example` ships a working example that
shells out to `notify-send` for a desktop notification, for both
shapes. A deploy that passes its check is quiet by default; a failed or
rolled-back one always runs the command. `NOTIFY_DEPLOY_OK=1` in
`plugins/notify/config` (see `config.example`) turns a passing deploy's
notification on too. It is a plugin in under a hundred lines and it
imports nothing. Install with `forge plugin install plugins/notify`.

**github-issues** (`intake`, `events`) files a task for every open issue
on a GitHub repository that carries a chosen label, quoting the issue
body into the task text with a note that it is data, not instructions,
and records the issue as a `ref` on the task it files. When that task's
`task_done` event arrives, it comments back on the issue with the
outcome, adding a configured label if the task landed. Its configuration
is `plugins/github-issues/config` (see `config.example`): which repo to
watch, the label, the repo and workflow new tasks are queued against,
and the poll interval. It hands text a stranger wrote to an agent, which
is safe to enable because an attempt's network is bounded: its only
route out is the egress proxy, which allows the model endpoint and the
hosts the repository's `forge.toml` declares under `[sandbox] egress`
(see `config.example` for what that means for this plugin). Install with
`forge plugin install plugins/github-issues`.

**signal** (`events`, `intake`, `message`) is a two-way bridge to Signal,
run as one process with two loops so either exiting stops both. Outbound
follows `forge events` the way notify does and messages a configured
Signal number or group when a task reaches a state on its watch list
(blocked, by default, or failed), including the blocked question if
there is one, when a deploy finishes (a failed or rolled-back deploy
always messages, a passing one only when `NOTIFY_DEPLOY_OK=1` is set),
and when `forge intake accept` creates a project for the first time for
a name in `CONTACTS` (docs/PORTAL.md, "Reachable"), sending that contact
their customer portal link unprompted. Inbound polls `signal-cli
receive`. An allowed sender or a `CONTACTS` name can answer a task
(`/answer <id> <text>`), report queue status (`/status`), or ask for the
command list (`/help`); an allowed sender's plain message files new work
via `forge add`, and a `CONTACTS` name can also text `/portal` at any
time to get their own portal link resent. Everything else from a
`CONTACTS` name — unless it answers their own open question, which is
submitted as their answer instead — routes through the concierge
(docs/INTAKE.md, "The front door is not the interview"): `forge ask
<project> "<message>" --from <name>`, the project being whichever
`PROJECTS` names for them, or else `TARGET_REPO`'s own project, and the
reply (an answer, "on it" for a filed task, or the question when the
decision is unclear) is sent back to them. Anyone else's message is
logged and dropped. Every message this plugin's own logic routes
through `forge ask` or `forge answer` — a contact's own words, in either
direction — and every reply it sends over `signal_send`, is recorded in
the message record (`forge message record`, see "message" above), so
`forge message list <project> --contact <name>` answers "has this
contact replied since" for this channel; a project it cannot name for a
message (e.g. an unaddressed operator notification whose task predates
projects) is skipped rather than failed, the same best-effort posture
as the portal link. The record call for a contact's inbound message is
what fires message triggers (docs/JOBS.md, "Triggers"): a run workflow
whose `[trigger]` is `on = "message"` with this contact, or `"*"`, is
started with the message as its input. Its configuration is `plugins/signal/config` (see
`config.example`): the bot's Signal account, who to notify, the allowed
senders, contacts and their projects, the target repo and workflow,
which states to notify on, whether a passing deploy is worth a message,
and the portal's public URL. Install with `forge plugin install
plugins/signal`.

**statusline** (`events`) maintains a status document, not a bar itself:
it writes `$XDG_STATE_HOME/forge2/status.json` atomically on every event
that changes the picture and on a five-second heartbeat otherwise, so a
widget can tell a stale file from an idle one. The document carries an
overall state (`attention`, `working`, or `idle`, derived from open
questions, a recent failure, or a running task, in that order), the
running tasks, queued and blocked counts, and the open question count.
Its configuration is `plugins/statusline/config` (see `config.example`),
which can set the forge-web URL to include in the document. Install with
`forge plugin install plugins/statusline`.

## What this is not

Not a way to extend the kernel: a plugin cannot add a verification rule,
a contract or a workflow step. Those are actions and directives, which
are versioned, hashed and measured because the kernel enforces them. A
plugin sits outside that boundary and watches.
