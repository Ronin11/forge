# Deploy targets

*2026-09-15. Forge owns deployment: what a target is, how a deploy runs,
and what is deliberately not built.*

A landed change that nobody can use is not done. Forge verifies and
lands; the last step, putting the landed thing where it runs, has been
the operator's by hand. This note makes it Forge's, without building a
continuous-integration system, and without deciding for a project what
"production" means. For one project it is a web host; for another it is
a scheduled job on a family member's laptop; for a third it is a
service on a machine in the next room. All three are the same shape.

## A target

A project declares targets. A target is four things:

- **Where**: a host. `local`, a name resolvable over SSH, a static host,
  or a hosted pipeline that does its own hosting.
- **How**: a method, which is a built-in action file like every other
  operation Forge runs, with the target's arguments.
- **What proves it is up**: a check command, run where the thing runs,
  whose exit status is the verdict. A URL to fetch is the common case
  and is a check command too.
- **What to roll back to**: the previous deploy that passed its check.
  Forge keeps this; the target does not have to.

Targets belong to projects, not repositories, because a project is the
thing that is *for* someone and a repository can serve several. A target
names the repository it deploys and, for a monorepo, the scope.

```
forge project deploy add <project> <name> --repo <path> --method <action> [--arg k=v]... --check <command> [--smoke <url>] [--on-landing]
forge project deploy set <project> <name> [--repo <path>] [--method <action>] [--arg k=v]... [--check <command>] [--smoke <url>] [--on-landing|--no-on-landing]
forge project deploy remove <project> <name>       refused while a deploy of it is running
forge project deploy list <project>
forge deploy <project> <name> [--sha <commit>]     run it now
forge deploy log <project> [<name>]                what was deployed when, and what the check said
```

`set` replaces only the fields given, leaving the rest as they were: an
`--arg` replaces or adds that one key in the args map rather than
clearing it, and `--check`, `--smoke` and `--on-landing`/`--no-on-landing`
each replace their own field the same way. `remove` deletes the target
outright, so a repository it was `--on-landing` for stops deploying it on
future landings; it is refused while a deploy of that target is still
running, since there would be nothing left to record the result on.

## Methods

A method is an action file under `src/builtins/operations/deploy-*.toml`,
so it is versioned, hashed and measured like every operation, and an
operator can add one in the workflows directory without touching Forge.
The first four are for a project's own thing; the fifth, `deploy-self`, is
for Forge:

- **`deploy-command`**: the generic one. Copy the landed tree (or the
  build output the repository's `build` check produces) to the host with
  rsync over SSH, then run a command there. The check runs there too.
  This covers a laptop in the house, a server, and most things between.
  An optional `exclude` arg, a comma-separated list of rsync exclude
  patterns, keeps paths like `node_modules` or `.next` on the host
  between deploys instead of rsync's `--delete` clearing them for the
  command to rebuild from scratch every time.
- **`deploy-user-service`**: `deploy-command` plus restarting a user-level
  systemd unit on the host and waiting for it to report active. The
  shape for "an automation that runs on her machine": the unit is a
  timer or a service, Forge replaces its files and restarts it, the
  check asks the service whether it is healthy. The same `exclude` arg
  applies.
- **`deploy-static`**: push a built directory to a static host: a
  branch a pages service serves, or a bucket. The check fetches the
  published URL and looks for a marker the build writes (the commit
  hash in a meta tag is enough).
- **`deploy-pipeline`**: trigger a pipeline the host runs itself (a
  workflow dispatch, a deploy hook) with the landed commit, then wait
  for it to report success and run the check. This is the whole
  integration with hosted CI: Forge triggers and waits; it does not
  reimplement.

- **`deploy-self`**: Forge on this machine, from a landing on its own
  repository. See "Deploying Forge itself" below.

Nothing here is an agent. A method is a script with arguments. An agent
may be given a task to *write* a deploy script for a project; running
it is the kernel's, deterministic, with a record.

## A deterministic smoke step

A check command proves a port answers, not that the site works: a map
provider's "API key required" placeholder is an image served with
status 200, so a check that fetches two endpoints cannot see it (this
is what happened to equitizr's map tiles the day it went live).

A target may declare `--smoke <url>`. After its check passes, `forge
deploy` opens that url in headless Chromium through the `deploy-smoke`
operation (`src/builtins/operations/deploy-smoke.toml`), launching the
browser the same way the `playwright` operation does, sandbox flags
included. It waits for network idle up to twenty seconds and records:

- every request that failed outright or came back with status 400 or
  above, its url and status, subresources included (a tile, a script,
  a font);
- every console error;
- the page title;
- a full-page screenshot, saved beside the deploy's record under
  `FORGE_HOME/deploys/<id>/`.

A console error or a failed request to the site's own origin fails the
deploy, exactly like a failed check: rollback and the human rung below
both apply. A failed request to a third party (an ad network, an
analytics beacon) is recorded but does not fail the deploy; a target
cannot promise what it does not control. Chromium writes its own
"Failed to load resource" console message for every failed request,
including third-party ones already listed among the failed requests;
that boilerplate is recorded but does not fail the deploy either, so a
target is never failed purely for a third party's outage. The result
lands on the deploy row as `smoke_ok` and `smoke_json`, and `forge
deploy log` shows it. A target with no `--smoke` url skips the step
entirely.

## The deploy look

A check that answers, and a smoke step that finds no console error or
failed request, still is not proof the page looks right: a map
provider's "API key required" placeholder is an image served with
status 200 and no console error at all, so nothing deterministic sees
it — only eyes do (this is the same equitizr map-tile failure "A
deterministic smoke step" above opens with; it can hide from a smoke
check too, if the placeholder itself never errors).

Whenever a target declares `--smoke <url>` and the smoke step leaves a
screenshot to look at, `forge deploy`'s last step is `deploy-look`
(`src/builtins/actions/deploy-look.toml`, see docs/ACTIONS.md, "The
deploy look"): a read-only agent given the screenshot, the page title,
the smoke step's console-error and failed-request lists, the target's
url, and the project's purpose, asked for a short verdict — `ok` and a
list of `findings`, each a `severity` of `blocking` or `notable` and one
sentence, judging only what the screenshot shows. It runs whether or not
the smoke step itself passed, since a passing smoke check is exactly the
case a placeholder or empty page can hide behind.

The verdict lands on the deploy row as `look_ok` and `look_json`, shown
by `forge deploy log` and the initiative report. A `blocking` finding
fails the deploy exactly like a failed check: rollback and the human
rung below both apply, the finding's sentence named in the question. A
`notable` finding is recorded but does not fail the deploy. A run that
fails on its own — the agent errors, or its result does not fit the
schema — is logged and ignored, never failing the deploy for it.

## When a deploy runs

After a landing on the target's repository, if the target was declared
`--on-landing`; otherwise on `forge deploy`. Landing and deploying are
separate steps with separate records on purpose: a landing can be good
and a deploy can still fail, and the record must say which.

A deploy is recorded like an operation: target, commit, started,
finished, the check's output and verdict, and what it rolled back to if
it did. It emits `DeployStarted` and `DeployFinished` events, so the
notify and signal plugins can say "equitizr is live at <commit>" or "the
deploy failed and rolled back", and the factory-floor page can show it.

## Provisioning

Declaring a target is one command; standing up the box it names is not —
by hand it is a firewall, a server, a wait, and editing `~/.ssh/config`,
each a place to get it wrong. `forge provision` collapses that into one:

```
forge provision <project> <name> [--arg k=v]...
```

`<name>` must already be a deploy target on `<project>` (`forge project
deploy add`), because provisioning is about the box, not the target's
method or check. It runs the `provision-hetzner` operation
(`src/builtins/operations/provision-hetzner.toml`), a script like every
other operation, never an agent:

1. Create a firewall named `<name>` if one by that name does not already
   exist, allowing TCP 22, 80, 443 and ICMP from anywhere.
2. `hcloud server create` with `--arg type` (default `cpx21`),
   `--arg location` (default `ash`), `--arg image` (default `debian-12`),
   the firewall, `--arg cloud_init`'s file as user data, and an
   `--ssh-key` for each name in the comma-separated `--arg ssh_keys`
   (keys already uploaded to the Hetzner project; this never creates
   one).
3. Wait for the server to report running.
4. Write a `Host` block naming it by its ipv4 address to
   `FORGE_HOME/provision/<project>/<name>/ssh-config`, a file the
   operator appends to their own `~/.ssh/config` — never written there
   directly, since that file is the operator's and Forge does not edit
   it unasked.

The server's ipv4 is then recorded as the target's own `host` arg, the
same field `--arg host=<ip>` sets by hand, so the very next `forge deploy
<project> <name>` reaches the box it just built.

`docs/ops/hetzner-equitizr-cloud-init.yaml` is the reference cloud-init:
a user, its ssh keys, a Caddy config reverse-proxying the app, and a
user-level systemd unit for it. One step in it matters beyond that one
project: Debian 12 ships Caddy 2.6.2 (2022), which fails against Let's
Encrypt's current ACME endpoints ("downloading certificate chain ...
404") and silently falls back to the staging CA, so the site never gets
a real certificate. `runcmd` replaces the packaged binary with the
current GitHub release before Caddy's first start, keeping the package's
unit and user; every Hetzner box provisioned this way needs the same
step until Debian ships a newer Caddy.

### The customer portal, reachable

`forge-portal` (docs/PORTAL.md) binds loopback on the operator's own
host, the same box that already runs `forge-web` and every project's
deploy targets, so it reaches the world the same way they do: a name in
the reverse proxy's config, pointed at its port. Add a site block to the
same Caddyfile the provisioned box already runs:

```
portal.example.com {
    reverse_proxy 127.0.0.1:7799
}
```

`docs/ops/forge-portal.service` is the systemd user unit that keeps
`forge-portal` running: install it at
`~/.config/systemd/user/forge-portal.service` on the operator's host,
`loginctl enable-linger` the user so it survives a logout, then
`systemctl --user enable --now forge-portal`. Installing it is by hand,
once; after that `deploy-self` restarts it with the rest of Forge.

## Deploying Forge itself

A landing on the Forge repository redeploys Forge the way a landing on
equitizr deploys equitizr: nobody rebuilds by hand, nobody restarts a
unit. It is one target on the `forge` project, declared once:

```
forge project deploy add forge self --repo ~/Projects/forge --method deploy-self --arg dest=$HOME/Projects/forge --on-landing
```

`--repo` is the repository the deploy is triggered by and whose commit is
built; `dest` is the checkout Forge runs from, the one `~/.local/bin`'s
symlinks and `deploy/forge-worker.service` point at. They are usually the
same directory. No `--check` is needed: `deploy-self` supplies its own.

`deploy-self` (`src/builtins/operations/deploy-self.toml`) runs, in the
landed tree's scratch archive, under a lock so two landings never build
over each other:

1. Copy the five release binaries in `dest/target/release` (`forge`,
   `forge-web`, `forge-portal`, `forge-repomap`, `forge-tui`) to
   `dest/target/release/previous/`. That directory always holds what ran
   before the current deploy.
2. `cargo build --release --workspace` with `CARGO_TARGET_DIR` set to
   `dest/target`, not the scratch archive, so the build cache is used and
   the binaries land where the symlinks and the units point. The archive
   has no `.git`, so the commit is handed to `build.rs` as
   `FORGE_BUILD_SHA` and `forge version` still names it.
3. `systemctl --user restart forge-web forge-portal`, then wait, bounded,
   for both to report active.
4. Run the check, retried while it fails, up to a bound. The default asks
   the web client the same thing an operator's browser does: `GET
   http://127.0.0.1:7788/tasks` with `Authorization: Bearer` and the token
   in `FORGE_HOME/web.token`, expecting 200. A target's own `--check`
   replaces it.
5. Only when the check passed, and last, `systemctl --user restart
   --no-block forge-worker`.

Args: `dest` (required), `url`, `units` (default `forge-web
forge-portal`), `worker` (default `forge-worker`) and `tries` (default 40,
half a second apart, for each wait). The method gets `FORGE_DEPLOY_SHA`
and `FORGE_HOME` from `forge deploy` like the rest of its environment.

**How a deploy survives its own worker restart.** An on-landing deploy is
not a separate process: the worker that landed the task calls
`deploy::run` from inside the landing (`deploy_on_landing`), so the
process being restarted is the one running the method. Three things make
that safe. `--no-block` returns as soon as systemd has queued the
restart, so the method finishes and exits before anything is stopped.
The worker's stop is a drain, not a kill (`deploy/forge-worker.service`
sends one SIGTERM to the main process, `KillMode=mixed`, and waits up to
`TimeoutStopSec=2400`): it claims nothing new and lets every running
attempt finish, and the landing that triggered the deploy is one of them,
so the smoke step, the deploy row, the `DeployFinished` event and the
on-landing assessment are all written by the old process before it exits.
And the restart is the last thing the method does, after the check, so a
build that fails never reaches the worker: the worker keeps running the
old binary and no attempt is interrupted. Its replacement starts on the
new binary. Nothing here signals the worker twice (a second SIGTERM
aborts running attempts), and a deploy that rolls back restarts it at
most once, from the rollback's own passing run.

**Rollback.** A build or check that fails restores the binaries in
`previous/` over the new ones (copy then rename, so a running binary is
never written to), restarts web and portal onto them, leaves the worker
alone and exits non-zero. The generic rollback below then redeploys the
last passing commit through the same method, and the project gets its
question. A first-ever deploy has no `previous/` to restore and no passing
commit to roll back to; the record says so.

The whole method has to finish inside the repository's
`check_timeout_secs`, a cold `cargo build` included; the build cache in
`dest/target` is what makes a warm one fit.

## Rollback and the human rung

If the check fails, Forge deploys the previous passing commit with the
same method and runs the check again. If that passes, the project gets
a blocked question: "the deploy of <commit> failed its check and was
rolled back to <previous>; here is the check's output". If the rollback
fails its check too, the question says so and nothing further is
attempted. Either way a person decides; the record has everything they
need. There is no third try.

## Secrets and hosts

(A method also gets `FORGE_DEPLOY_SHA`, the commit it deploys, and
`FORGE_HOME`, since an operation's environment is otherwise cleared to
`PATH`, `HOME` and the like.)

A method gets its host credentials the way a plugin gets its
configuration: from the operator's environment or a file the target
names, injected into the operation's environment, never into a prompt
and never into a log. SSH uses the operator's agent. A hosted pipeline's
token lives in the operator config under the target and nowhere else.

## The laptop case, concretely

Project `household`, repository `~/Projects/household-automations`,
target `mary-laptop`: method `deploy-user-service`, host `mary-laptop`
over SSH on the home network, unit `household-automations.timer`, check
`systemctl --user is-active household-automations.timer`, on landing.
When a task lands, Forge copies the tree to her machine, restarts the
timer, asks whether it is active, and records the answer. If her machine
is off, the deploy fails its check, rolls back to nothing (there is no
previous deploy to restore, so it records that), and the project asks
the operator. When she turns it on, `forge deploy household mary-laptop`.

## What is not built

No pipeline definition language, no build matrix, no artifact store, no
environments-as-a-concept beyond the target's name. A target that needs
a staging step is two targets. Blue-green, canaries and progressive
rollout belong to the hosted pipelines that already do them; Forge
triggers and waits.

## Build order

1. The deploy record: a `deploys` table, the two events, `forge deploy
   log`.
2. `deploy-command` and the target verbs, with rollback and the
   question, tested against a fake host (a directory on the same
   machine reached by a fake `ssh` on `PATH`).
3. `deploy-user-service`, tested the same way with a fake `systemctl`.
4. `deploy-static` and `deploy-pipeline`, each with a fake.
5. The on-landing hook in `landing.rs`, and the deploy on the initiative
   report.

Equitizr is the first user: a `deploy-static` or `deploy-pipeline`
target to wherever it is hosted, on landing, with its data-freshness
endpoint as the check.
