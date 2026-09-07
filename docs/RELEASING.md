# Releasing

Forge ships as prebuilt binaries on GitHub Releases. The pipeline is two
workflows under `.github/workflows/` and two scripts under `scripts/`; every
piece runs locally too.

## The pipeline

| Trigger | Workflow | What runs |
| --- | --- | --- |
| push to `main`, any PR | `ci.yml` | `just check` (the full gate), then `just cross` (every release target must compile) |
| push a tag `v*` | `release.yml` | `ci.yml` first, then `scripts/release.sh`, `scripts/release-notes.sh`, `gh release create` |

Release artifacts, per tag:

| Asset | Contents |
| --- | --- |
| `forge_linux_amd64.tar.gz` | `forge` + README |
| `forge_linux_arm64.tar.gz` | |
| `forge_darwin_amd64.tar.gz` | |
| `forge_darwin_arm64.tar.gz` | |
| `checksums.txt` | SHA-256 of each archive |

Builds are `CGO_ENABLED=0 -trimpath -ldflags "-s -w -X main.version=<tag>"`,
so `forge version` prints the tag and the binaries are static. Asset names
carry no version on purpose: `https://github.com/Ronin11/forge/releases/latest/download/<asset>`
is a stable URL, which is what the site's download table and `install.sh`
(served at https://crashbyforge.com/install.sh) rely on. The version lives in
the tag and inside the binary.

## Cutting a release

```bash
just check                      # the gate must be green locally first
git tag -a v0.2.0 -m "v0.2.0"
git push origin v0.2.0
```

Nothing else. The release appears at github.com/Ronin11/forge/releases within
about ten minutes with generated notes: `feat`/`fix`/`perf`/`refactor`
subjects since the previous tag, grouped, plus the download table. A tag with
a hyphen (`v0.3.0-rc1`) is published as a pre-release, so `/releases/latest/`
keeps pointing at the last stable one.

Dry-run the whole thing locally:

```bash
just dist v0.2.0                # → dist/*.tar.gz + checksums.txt
just release-notes v0.2.0       # the markdown the release will carry
```

To re-cut a tag (the gate failed and you fixed main): delete the tag locally
and remotely, re-tag, push. `gh release create --verify-tag` refuses a release
whose tag is missing, and a re-run on an existing release fails rather than
overwriting it — delete the release first (`gh release delete v0.2.0`).

## Prerequisites and secrets

- **Submodules.** CI checks out all three (base library for `go:embed`, site,
  plugins). Their URLs are ssh; `actions/checkout` rewrites them to https with
  its token. `GITHUB_TOKEN` can only read *this* repository, so while
  `forge-library`, `forge-plugins`, and `ashbyforge` are private, a
  fine-grained PAT with **contents: read** on all four repositories must live
  in the `SUBMODULE_TOKEN` repository secret (`gh secret set SUBMODULE_TOKEN`).
  Once the submodule repositories are public the fallback token suffices and
  the secret can go.
- **Visibility.** Release assets on a private repository are visible only to
  collaborators: the site's download links and `install.sh` return 404 to
  everyone else until `Ronin11/forge` is public. Everything else in this
  pipeline already works on a private repository.
- **No signing.** macOS binaries are unsigned and un-notarized; a browser
  download gets the quarantine bit (`xattr -d com.apple.quarantine forge`),
  a `curl` download does not. Signing needs an Apple Developer account and is
  not wired.
- **Playwright.** `just ui-test` skips itself without chromium; CI installs it
  (`npx playwright install --with-deps chromium`) so the browser tests run.
- **Runner speed.** A GitHub-hosted runner is far slower than the reference
  laptop, and two recipes are calibrated against it. `just test` runs with
  `-timeout 40m` because `store` and `web` cross `go test`'s 10-minute default
  there, and CI sets `BENCH_SCALE=8` so `bench/threshold.txt`'s laptop ceilings
  are read relative to the runner (BenchmarkInsertEvents: 0.33 ms/op on the
  laptop, 2.08 ms/op on a runner). The tight benchmark gate stays local; CI
  catches only a catastrophic regression.

## Windows

Not built. The daemon's process model is POSIX end to end and these do not
compile on Windows:

| Mechanism | Where |
| --- | --- |
| process groups: `Setpgid`, `Setsid`, `Kill(-pgid)`, `Getpgrp`/`Getpgid` | `cmd/forge/{cmd_daemon,run_supervisor}.go`, `internal/core/worker/{supervisor,sweep,verify,git}.go`, `internal/tools/mcpserve/local.go`, `internal/tui/cli_client.go` |
| `Flock` on the daemon/worker lock files | `cmd/forge/cmd_daemon.go`, `internal/core/daemon/daemon.go`, `internal/core/worker/runner.go` |
| `syscall.Exec` in-place restart + `FD_CLOEXEC` fcntl | `cmd/forge/cmd_daemon_restart.go` |
| `SIGUSR1` log reopen, `SIGHUP`/`SIGSEGV`/… exit classification | `internal/core/logging/signal.go`, `internal/core/worker/supervisor.go` |
| `Statfs` disk-space checks | `internal/core/doctor/local.go`, `internal/web/handlers_health.go` |
| `Umask`, `O_NOFOLLOW` | `internal/core/daemon/daemon.go`, `internal/core/worker/{manifest,supervisor}.go` |

A port means `_unix.go` / `_windows.go` splits for each (job objects for
process groups, `LockFileEx` for flock, a spawn-and-exit restart instead of
exec-in-place, `GetDiskFreeSpaceEx` for statfs), plus deciding what the
unix-socket API and `~/.forge` layout look like there. Until then the
documented answer is WSL2 with the Linux build. To add it to the pipeline
once it compiles: append `windows/amd64` to `TARGETS` in `scripts/release.sh`
and to `just cross`, and give the archive a `.zip` with `forge.exe`.
