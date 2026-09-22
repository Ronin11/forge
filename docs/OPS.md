# Operating the store

*2026-09-22. What runs unattended against the operator's own Forge home,
and what to do when it does not, plus how a release is built. docs/CHECKS.md
covers the standing checks (doctor, drift); `backup-daily` below is the job
that guards the data.*

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

There is no `.github/workflows/release.yml` yet — this repository has no
`.github` directory. Once it exists, add a workflow triggered on a `v*` tag
that runs `scripts/release.sh` (one job per target triple in the matrix)
and uploads each `dist/forge-*.tar.gz` and `dist/SHA256SUMS` as release
assets. Until then, cut a release by hand: tag the commit `v<version>`
matching `Cargo.toml`'s `[package] version` (so `forge version` names the
same tag), run `scripts/release.sh` for each target the release ships, and
attach the resulting `dist/` files to the tag.
