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
capabilities = ["events"]                # events | intake
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

A plugin may declare both, and most useful ones do: watch for a blocked
task, ask a person, file the answer with `forge answer`.

**Not yet: tools and annotate.** Forge 1 aggregated plugin MCP servers
into the tool list the agent sees. Forge 2's agent is the claude CLI,
which loads its own MCP servers, so the shape here would be a generated
per-task config rather than an aggregator, and it is deferred because a
plugin tool with network access is a new path out of what the
verification rules can see, which deserves its own design. External
references on a task (the pull request, the issue) are deferred to the
task that adds them to the store and the client contract.

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

## The reference plugin

`plugins/notify/` in this repository is the one to copy. It is a shell
script: it takes a snapshot, subscribes from its offset, and runs a
command of the operator's choosing when a task blocks on a question,
fails, or lands. It keeps its cursor in `FORGE_PLUGIN_STATE`, so a
restart resumes where it stopped. It is a plugin in thirty lines and it
imports nothing.

## What this is not

Not a way to extend the kernel: a plugin cannot add a verification rule,
a contract or a workflow step. Those are actions and directives, which
are versioned, hashed and measured because the kernel enforces them. A
plugin sits outside that boundary and watches.
