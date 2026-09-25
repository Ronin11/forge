# Operating the store

*2026-09-22. What runs unattended against the operator's own Forge home,
and what to do when it does not, plus how a release is built and installed.
docs/CHECKS.md covers the standing checks (doctor, drift); `backup-daily`
below is the job that guards the data.*

## When the event log cannot be written

`FORGE_HOME/events.jsonl` (the record `forge job show` and every client's
subscription read) is written best-effort: a directory that is briefly
full or wrongly permissioned never fails the attempt whose event it was.
The first write that fails for a task prints one `Note` on stderr naming
the path and the error, and never repeats for that task — an attempt
stuck in a bad environment does not spam its own log. `forge doctor`'s
`logs` check counts how many distinct tasks hit this, adding `N task(s)
lost log lines` to its detail and its hint (`check disk space and
permissions for events.jsonl`) once `N` is above zero, so a systemic
problem is visible even though no single attempt reported it as a
failure.

## Installing and upgrading

`forge init [--home DIR]` is what a second machine runs once, against the
binaries unpacked from a release archive (see "Releases" below), to get a
working `FORGE_HOME` (default `~/.local/share/forge`, the same resolution
`Paths::resolve` always uses — `--home` overrides it for that one run):

1. Creates the data directory (`worktrees/` and `logs/` under it).
2. Writes `config.toml` from `config::DEFAULT_HOME_CONFIG` — never
   overwriting one that is already there.
3. Makes `FORGE_HOME/workflows` a git repository (if it is not one yet),
   writes every built-in action, operation and workflow that is not
   already present, and commits whatever that leaves uncommitted as
   Forge — so a fresh install's catalog starts at a clean, committed
   `git status`, the same thing `forge doctor`'s `workflows` check counts.
4. Generates `web.token` (32 bytes of `/dev/urandom` as hex, mode 0600) if
   it does not exist — the same generator `forge web link` falls back to.
5. Writes `forge-worker.service` and `forge-web.service` under
   `$XDG_CONFIG_HOME/systemd/user` (`~/.config/systemd/user` when
   `XDG_CONFIG_HOME` is unset — the OS user's own config directory, never
   `FORGE_HOME`, which may sit elsewhere), each `ExecStart=` pointing at
   the currently running `forge` binary's own path (`forge-web` is
   expected beside it), with `Environment=FORGE_HOME=` set to the home
   just set up. When a systemd user session is reachable (`sd_booted()`:
   `/run/systemd/system` exists, plus `XDG_RUNTIME_DIR`, which a login
   session sets), it also runs `systemctl --user daemon-reload`,
   `systemctl --user enable --now <worker unit path> <web unit path>`
   and `loginctl enable-linger`, so both survive a logout — `enable` is
   given the absolute paths of the two files this run just wrote, never
   the bare unit names, so it can only ever act on those two files, not
   on whatever `systemctl --user` would otherwise resolve a bare name to.
   Without a session, the unit files are still written, and the same
   three commands (with the same paths) are printed instead of run, for
   the operator to run by hand once one is available. Nothing under
   `install_units` runs unless both conditions hold, so a plain
   `cargo test` — no session, `XDG_RUNTIME_DIR` unset — only ever prints;
   `tests/e2e/init.rs` additionally strips `XDG_RUNTIME_DIR` and
   `DBUS_SESSION_BUS_ADDRESS` from its own no-session test's environment,
   so the suite exercises the print path even when run inside a desktop
   session where both are set.
6. Ends by running the same checks `forge doctor` reports (against the
   home `forge init` just set up, even with `--home`), so a missing
   `bwrap`, `git` or a `claude` CLI that is not logged in is named on the
   spot rather than on the first real attempt.

Every step only changes what is not already exactly right — the config is
never overwritten, a built-in already on disk is never rewritten, a unit
file identical to what would be written again is left alone and neither
`systemctl` nor `loginctl` is re-run — so running `forge init` again on an
already set up machine reports every step unchanged and says so
(`already initialized; nothing changed`), the same shape `tests/e2e/init.rs`
holds: creation the first time, "nothing changed" the second, and
`--home` never touching the default `FORGE_HOME`.

### Upgrading

```
forge upgrade [<path-or-url>] [--check-only] [--force]
```

`<path-or-url>` names a release archive built by `scripts/release.sh`
("Releases", below): a local path, or a URL, fetched with `curl` into a
scratch directory under `FORGE_HOME/worktrees/upgrade/`. Either way, a
`SHA256SUMS` naming the archive must sit beside it — the same directory
for a path, the same URL with `SHA256SUMS` in place of the archive's own
name for a URL — and the archive is refused if its hash does not match.
Omitting `<path-or-url>` is refused too; there is no default source.

1. **Verify.** The archive's SHA-256 against its `SHA256SUMS` entry.
2. **Version-gate.** The version is read from the archive's own file name
   (`forge-<version>-<target>.tar.gz`, exactly what `scripts/release.sh`
   names it), never by running the binary inside before it is verified.
   A version older than the running one (`forge version`) is refused —
   migrations are forward-only, so downgrading the binary over a store a
   newer schema has already migrated is not a supported path — unless
   `--force`, which installs it anyway and says so. `--check-only` stops
   here, after verifying and reporting the version, without installing,
   backing up or restarting anything.
3. **Extract** and confirm the five release binaries are all present.
4. **Read the schema version** the running store is at (`PRAGMA
   user_version`, via `Store::schema_version`).
5. **Back the store up**: `sqlite3 FORGE_HOME/forge.db .backup` (the same
   online, WAL-safe snapshot `backup-store.toml` takes) into
   `FORGE_HOME/backups/<version>-<unix time>/forge.db`.
6. **Keep the current binaries.** The five release binaries
   (`forge`, `forge-web`, `forge-portal`, `forge-repomap`, `forge-tui`) in
   the running binary's own directory are copied into `<bin
   dir>/previous/` — copy then rename, so a binary already running is
   never written to — before anything is overwritten.
7. **Install** the new binaries over the old ones, the same copy-then-
   rename way.
8. **Migrate.** The newly installed `forge doctor --json` is run against
   `FORGE_HOME` — opening the store once, so it is the new binary's own
   migration ladder that runs, forward-only, never the old binary's —
   and its `schema` check's version is read back and reported beside the
   version from step 4, so an operator sees exactly what moved.
9. **Restart `forge-web` and `forge-portal`**, each only if its unit file
   exists under `~/.config/systemd/user` (a satellite worker box may have
   neither), and wait for the ones restarted to report active.
10. **Check the web client**, the same default `deploy-self` uses: `GET
    http://127.0.0.1:7788/tasks` with `Authorization: Bearer` and the
    token in `FORGE_HOME/web.token`, expecting 200, retried up to 40
    times half a second apart — run only when `forge-web`'s unit existed
    and was restarted onto the new binary in the previous step.
11. **Ask `forge-worker` to restart**, last, and only once its unit
    exists: `systemctl --user restart --no-block forge-worker`, so it
    drains its running attempts (this command's own process, were it the
    worker) before coming back on the new binary.

Any failure from step 6 onward — a bad build, a schema check the new
binary fails, a unit that never becomes active, a web check that never
passes — restores the binaries kept in step 6 over the current ones and
says so; a rollback restarts `forge-web`/`forge-portal` onto them but
never touches `forge-worker`, so it is left running the binary it
already trusted rather than restarted onto one that failed its check
(the same shape `deploy-self`'s own rollback holds, docs/DEPLOY.md,
"Deploying Forge itself", "Rollback."). A failure before step 6 (a bad
checksum, an older version without `--force`) changes nothing at all.

`forge upgrade` shares deploy-self's shape — snapshot to `previous/`,
restart web and portal, wait, check, restart the worker last — but not
its text. `deploy-self` (`src/builtins/operations/deploy-self.toml`) has
to be one self-contained script that runs from an archived checkout of
whatever repository a deploy target names, under the operation kernel's
`FORGE_ARG_*` contract; nothing guarantees that tree is Forge's own
source (the e2e suite deploys a throwaway fixture repository through
it), so it has no file it can reliably `source`. `forge upgrade` is a
first-class CLI command with its own tarball, `SHA256SUMS` and backup
steps that have no home in that generic bash contract either. The two
are kept honest against each other by mirroring the same step order and
restart/check/rollback semantics in `src/upgrade.rs`, rather than by
literally sharing one file; `tests/e2e/upgrade.rs` and
`tests/e2e/deploy.rs`'s `SelfDeploy` fixture hold the two shapes to the
same behaviour.

## `backup-daily` — 03:30 UTC daily

The store (`FORGE_HOME/forge.db`) is the only copy of every task, job,
decision and measurement, and `FORGE_HOME/config.toml` is the operator's
own configuration. Once a day, `.forge/workflows/backup-daily.toml` (a run
workflow on the Forge project, `cron = "30 3 * * *"`) runs one operation,
`.forge/workflows/actions/backup-store.toml`:

1. `sqlite3 forge.db .backup` into a scratch directory. `.backup` is
   SQLite's online backup, a consistent snapshot while the worker keeps
   writing; a plain `cp` of a live WAL-mode database is not.
2. `rsync -a -e ssh` of that copy and `config.toml` to
   `equitizr:~/backups/forge/<UTC date>/`. `equitizr` is a host in the
   operator's `~/.ssh/config`; the job's environment carries `HOME` and
   `PATH` but not `SSH_AUTH_SOCK`, so the key it names must be usable
   without an agent.
3. `ssh equitizr sqlite3 ~/backups/forge/<date>/forge.db 'pragma
   integrity_check'` must print `ok`. Anything else fails the job. The
   receiving host needs `sqlite3` installed for this check; the first run,
   on 2026-09-21, failed on a box without it.
4. Only then, every dated directory under `~/backups/forge/` beyond the
   seven newest is removed. Names that are not `YYYY-MM-DD` are never
   touched. Running twice on one day (`per_day = 2`) overwrites that
   day's directory and does not use up a slot.

A store with no `config.toml` runs on defaults, so there is nothing to
copy and that file is skipped; a missing `forge.db` fails the job.

The operation logs one `file` effect per copy, target
`equitizr:~/backups/forge/<date>/forge.db` and `.../config.toml`, so
`forge job show <id>` says what left the box. The workflow asserts at
least one file effect. `[limits]` are `budget_usd = 0` (no model is
involved), `per_day = 2`, `on_failure = "ask:operator"`: a failed backup
files a blocked request naming the job, which `forge requests` shows.

`BACKUP_HOST` (default `equitizr`) and `BACKUP_KEEP` (default `7`) in the
job's environment override the host and the count.

### Trying it and restoring

```
forge job start forge backup-daily --now --dry-run    logs what it would copy; runs no sqlite3, ssh or rsync
forge job start forge backup-daily --now              take one now
forge job show <id>                                   the effects, the state
```

A dry run logs one `would copy` file effect per copy, touches nothing
remote, and never dials the host.

To restore, stop `forge-worker`, `forge-web` and `forge-portal`, copy
`~/backups/forge/<date>/forge.db` from the equitizr host over
`FORGE_HOME/forge.db` (removing any `forge.db-wal` and `forge.db-shm`
beside it), put `config.toml` back, and start the units.

The e2e tests in `tests/e2e/jobs.rs` (`backup_daily_*`) hold the file
formats and the behaviour above: the workflow parses, a dry run records
its effects and dials nothing, and a real run against a fake remote
copies, verifies and prunes to seven.

## Releases

`scripts/release.sh [target-triple]` builds the workspace in release mode
and packs one archive: `forge`, `forge-web`, `forge-portal`, `forge-tui`,
`forge-repomap` (and `forge-test`, once that crate exists),
`deploy/forge-worker.service`, `docs/ops/forge-web.service` and a
`config.toml` template, into `dist/forge-<version>-<target>.tar.gz` beside
`dist/SHA256SUMS`. The target defaults to the host `rustc` reports; naming
a different one assumes its toolchain is already installed and passes it
to `cargo build --target`.

The version is never typed by hand: the script reads it back from the
binary it just built (`forge version`), so the archive's own name — and
the tag a release should carry — is exactly what `forge version` reports
on every install and upgrade built from it, never a copy that can drift
from `Cargo.toml`. The `config.toml` template is built the same way: the
script points `FORGE_HOME` at a scratch directory and runs the freshly
built `forge project list` (a read-only command, no network or agent
needed) so `Forge::open` writes it exactly as `config::DEFAULT_HOME_CONFIG`
does, then copies it into the archive — one source of truth, not a hand-kept
copy.

`tests/release.rs` derives the workspace's own binaries from every member's
`Cargo.toml` and fails `cargo test --workspace` if the script's `BINS=` list
drifts from them, so a new crate that ships a binary cannot be forgotten.

`.github/workflows/release.yml` runs on a `v*` tag: one job per target
triple in its matrix (`x86_64-unknown-linux-gnu` today), each running
`scripts/release.sh` for its target, checking that the tag equals the
version the built binary reports, and attaching `dist/forge-*.tar.gz` and
`dist/SHA256SUMS` to a GitHub release of the tag's name. To cut one: bump
`[package] version` in Cargo.toml and land it, then tag that commit
`v<version>` and push the tag to origin — the bare repository's
post-update hook mirrors `main` and `v*` tags to GitHub, so the release
builds from there without a hand push. `forge upgrade
https://github.com/Ronin11/forge/releases/download/v<version>/forge-<version>-<target>.tar.gz`
installs it on another machine.


### When the worker dies

Jobs record the PID of the process that claims them. On startup, the worker
recovers running jobs whose owner has died (including old rows without an
owner). A job with no recorded effects returns to the queue with a recovery
step naming the previous worker. A job with any recorded effects ends failed;
its verdict names the previous worker and every recorded effect. Those jobs
are never automatically rerun. The workflow's `on_failure = "ask:operator"`
or `"ask:contact"` files the usual question; `drop` leaves the failed record,
and `retry:N` files an operator question instead of repeating external work.
The second-signal abort path applies the same rule. Inspect `forge job show`,
`forge job log`, and `forge requests` to reconcile interrupted effects.

Recovery uses the persisted effect rows; it cannot identify an external effect
that happened before its row was recorded. PID ownership also cannot distinguish
a dead process from an unrelated process that has reused its PID.

## Environment needs

A missing tool is a policy decision, never a human question. When an
operation step (setup, a check) or an attempt fails and its output tail, or
the question the agent asked, names an environment need, the kernel
recognizes it (`src/environment.rs`, pure code) as one of:

- a **host** the egress proxy refused (its 403 line, `forge egress:
  HOST:PORT is not allowed`, or a tool's `403` line naming a URL);
- a **binary** missing from PATH;
- a missing **toolchain**;
- a missing **cache** under `~/.cache` (a Playwright browser, node headers).

`[environment]` in `config.toml`, beside the egress policy in
`docs/SYSTEM.md`, says which needs are granted without asking:

```toml
[environment]
hosts = ["registry.npmjs.org", "index.crates.io", "static.crates.io", "nodejs.org",
         "cdn.playwright.dev", "playwright.azureedge.net",
         "playwright-akamai.azureedge.net", "playwright-verizon.azureedge.net"]
cache_paths = ["~/.cache/node-gyp", "~/.cache/ms-playwright"]
```

Those are the defaults; a key you write replaces its list (`hosts = []`
grants no host). A refused host on the list is added to that worktree's
egress, and a missing cache under a listed path is mounted read-only into
its sandbox. Either way the kernel records a decision row answered by
`forge` (kind `environment-grant`) naming the need and the evidence line,
and runs the step or attempt again. That run is not a retry: nothing is
counted against the task, and each grant applies once, so a need that
survives its grant fails as it would have. A task at a trust level whose
egress is `model` never gets a host. Binaries and toolchains are recognized
but never granted here, and a need the table does not cover is left exactly
as it was: the failure, or the question, reaches the operator.

`forge doctor` lists every automatic grant of the last 7 days under
`environment`.

### A need the table does not cover

A host or cache need the `[environment]` table does not cover goes to the
supervisor (`src/env_supervisor.rs`), not the operator, when the supervisor is
on. It is given the typed need, the evidence line, the table and a ceiling it
may not exceed: **one named host** (never a wildcard, never `github.com`,
never a model endpoint), or **one directory under `~/.cache`**, read-only.
It approves or denies with a one-line reason.

- An approval within the ceiling is applied like an automatic grant, the run
  repeats without spending a retry, and the decision row (kind
  `environment-grant`) is answered by `supervisor`; `forge doctor` lists it
  with the automatic grants, marked as approved by the supervisor. It counts
  against the per-lineage supervisor budget (`[supervisor] per_lineage`).
- A denial, an approval past the ceiling, a failed supervisor run, or a
  lineage over its budget reaches the operator as a question on the blocked
  task (`forge requests`) that carries the need, the evidence and the reason,
  and asks yes or no. The answer is recorded as a decision; to make a yes
  stick, add the host or path to `[environment]` in `config.toml`, since the
  retried task meets the same refusal.
- Binaries and toolchains are never the supervisor's; they are left as before.

The ceiling is code, not prompt text. The repository may narrow it in its
`forge.toml`, read from the trusted base like the rest of the file:

```toml
[environment]
deny = ["*.example.com", "~/.cache/secrets"]   # hosts, `*.suffix` hosts, cache paths
```

The operator widens what needs no asking by widening the `[environment]`
table in `config.toml` (above); that table is never consulted by the
supervisor's judgement, only read by code.
