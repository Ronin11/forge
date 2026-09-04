# Forge modularization plan

Forge is one Go module (`forge`, `go 1.27`) that ships one binary. That binary is the
daemon, the CLI, the worker, and the MCP server at once: `cmd/forge/main.go` builds a
table of 24 subcommands and dispatches. This document plans the split of that module
into four named parts — **core**, **tools**, **web**, **tui** — decides the mechanism,
fixes the dependency direction, maps every package to its target, and sequences the
migration so `just check` is green at every step.

Companion documents: `DESIGN.md` (what the system is), `STYLE.md` (the code standard
this plan must not violate), `CONSTITUTION.md` (fixed principles), `VERIFICATION.md`
(how a change is proved).

Nothing here is a behaviour change. Every step is a move, a rename, or a mechanical
type split. The HTTP API, the SQLite schema, the plugin wire contract, and the command
tree are all invariant across the whole migration.

**Status (2026-09-01): stages 0–7 executed**, `just check` green at every commit.
Deviations from the letter of the plan, each argued in its commit message:

- Stage 2: `Server` **embeds** `*Engine` rather than holding the named `eng` field —
  promotion is what keeps every handler body and the ~30 direct test call sites
  compiling untouched. The named field lands when `Engine` is extracted to
  `core/engine` (below).
- Stage 5: `ui` **imports** `web` (a legal web→web edge) rather than the reverse —
  the shared read-path surface (repo/lineage view builders) is woven through the JSON
  handlers, so it was exported in place instead of moved; `Server.MountRoot` replaces
  `MountUI` and `cmd/forge` wires the two. `AttentionDeadline` moved to `core/engine`.
- Stage 6: the backup machinery moved from `web/handlers_health.go` to
  `core/engine/backup.go` (it was the one tui→web production edge); `plugin_omarchy.go`
  followed `cmd_plugin` into `tui`; the build version travels on `tui.Context.Version`.

**Still open, by design:** the deep cut of the `Engine` type and its method files
(`apply`, `prompt`, `route_claim`, `autoeval`, `ab`, `sweeper`, `attention`,
`supervision*`, `facts`, `handlers_plan`, drain's non-HTTP half) out of `web` into
`core/engine` — that is the embed→named-field conversion this plan defers; and §6.3's
DTO promotion into `protocol`, recorded as the one linter exception.

---

## 1. What is actually here today

### 1.1 One module, one binary, no nested modules

`go.mod` at the repository root is the only `go.mod` in the tree. The six first-party
plugins under `plugins/` (`status-file`, `notify`, `github-issues`, `signal`, `teams`,
`email`) are `main` packages **in this same module** — `just build-plugins` builds each with `cd
plugins/X && go build`, and their import paths are `forge/plugins/X`. They import
nothing from `forge/internal`. `ui/` at the root is the Playwright harness (Node), not
Go.

Sizes, non-test / test Go lines:

| tree | non-test | test |
| --- | ---: | ---: |
| `cmd/forge` | 7,801 | 2,308 |
| `internal/controlplane` | 12,830 | 7,416 |
| `internal/worker` | 6,366 | 3,147 |
| `internal/store` | 5,237 | 2,318 |
| `internal/tools` | 1,193 | 821 |
| `internal/mcpserve` | 910 | 487 |
| `internal/logging` | 785 | 458 |
| `internal/stats` | 647 | 565 |
| `internal/plugin` | 603 | 402 |
| `internal/integrator` | 600 | 449 |
| `internal/model` | 593 | 244 |
| `internal/eval` | 547 | 174 |
| `internal/kb` | 443 | 187 |
| `internal/doctor` | 446 | 331 |
| `internal/protocol` | 439 | 0 |
| `internal/modes` (+ 17 subpackages) | 1,248 | 345 |
| **total (`cmd` + `internal`)** | **40,688** | **19,652** |

### 1.2 The current import graph

Production imports only (`go list -f '{{.Imports}}'`), Forge packages only:

```
model            → —
protocol         → model
logging          → —
kb               → —
eval             → —
plugin           → model
stats            → model, store
store            → kb, model, protocol
worker           → logging, model, protocol
doctor           → plugin, store
mcpserve         → logging, protocol, worker
integrator       → model, protocol, store, worker
modes            → model, protocol
modes/schema     → worker
modes/<mode>     → model, modes, modes/schema, protocol
tools            → kb, model, protocol, stats, store
controlplane     → doctor, eval, kb, logging, model, modes, plugin, protocol, stats, store, tools
cmd/forge        → everything above
```

The graph is already acyclic and already has a floor (`model`, `protocol`) and a
ceiling (`controlplane`, then `cmd/forge`). `mcpserve` appears in `controlplane`'s
*test* imports only — a test speaks the MCP wire; there is no production edge.

Three of those edges are enforced by `just boundary`, which proves them with `go list
-deps`:

1. no package under `internal/worker/...` may reach `controlplane` or `store` (one
   SQLite writer),
2. `internal/model/...` may import no other Forge package,
3. `internal/protocol/...` may import nothing but `model`.

### 1.3 `internal/controlplane` is the problem, and it is two things

12,830 non-test lines in one package, 49 test files (7,416 lines) all declared `package
controlplane` — every one of them reaches unexported identifiers.

Splitting the files by whether they import `net/http` separates the package almost
exactly in half:

| | non-test lines | content |
| --- | ---: | --- |
| imports `net/http` | 8,009 | `server.go`, 16 `handlers_*.go`, `ui.go`/`ui_kb.go`, `stream.go`, `rpc.go`, `drain.go`, `routine_templates.go` |
| does not | 4,821 | `budget.go`, `queue.go`, `scheduler.go`, `router.go`, `facts.go`, `apply.go`, `route_claim.go`, `autoeval.go`, `ab.go`, `prompt.go`, `prune.go`, `sweeper.go`, `config.go`, `config_models.go`, `daemon.go`, `bootstrap.go`, `ui_actions.go`, `plugintools.go`, `handlers_plan.go` |

Of the non-HTTP half, ten files (2,425 lines: `bootstrap`, `budget`, `config`,
`daemon`, `facts`, `prune`, `queue`, `router`, `scheduler`, `ui_actions`) mention
`*Server` **zero times**, and two more (`config_models`, `plugintools`) mention it only
in a comment. Nine of those ten are already a separable core; they just live in the
same directory as the web server. (`ui_actions` is UI-side and goes to `web`.)

The obstacle is the god object. `Server` has 55 fields and 193 methods (non-test), and
`ServerOptions` has 39 fields, 20 of them `func` seams that `cmd/forge` injects
(`RegisterRepo`, `AddRepo`, `StartApp`, `ExecRestart`, `PluginStart`, `ModelCall`,
`EvalFn`, …). Roughly 60 of those methods never touch an `*http.Request` — they are the
engine — and the rest are handlers.

### 1.4 The UI is a store client, not an API client

`ui.go` (861 lines) + `ui_kb.go` (207) + `ui_actions.go` (83) render 20 embedded
templates under `internal/controlplane/ui/` (plus `ui/static`) with `html/template`,
and they read **the store directly** — 24 distinct `store` methods
(`ListWorkPage`, `TargetsForWorks`, `OpenQuestions`, `SearchKb`, …). The browser UI is
not an HTTP client of the JSON API; it is a second read path over the same database, in
the same process. A `web` module therefore has to contain *both* the JSON API and the
server-rendered UI. Turning the UI into an API client is a rewrite, not a move, and is
explicitly out of scope here.

### 1.5 There is no TUI, and the CLI is entangled with the daemon

Nothing in the repository uses a terminal-UI library; the only matches for "tui" are
the substring in `ui.go`'s neighbours. The current terminal surface is `cmd/forge`:
46 files (29 non-test, 7,801 lines), where each subcommand is a thin HTTP client built by
`cli_client.go` (388 lines) over the Unix socket, exactly as `DESIGN.md` §1 requires.

Two entanglements block lifting that out cleanly:

- **`cmd/forge` is also the daemon host.** It references 44 distinct `controlplane`
  identifiers. `cmd_daemon.go` (1,038 lines) is the only file in the repository that
  calls `store.Open` (`cmd/forge/cmd_daemon.go:173`); it constructs the registries, the
  `Server`, the `UI`, the plugin supervisor and the integrator. That is the composition
  root `STYLE.md` §1 and §3 require, and it must stay in `cmd/forge`.
- **Store row types are the CLI's wire types.** Eight command files decode API
  responses into `store.Target`, `store.Routine`, `store.Workflow`, `store.Repository`,
  `store.Proposal`, `store.Work`, `store.Question`, `store.KbNote`, `store.KbLink`,
  `store.JournalEntry`, `store.Attempt`, `store.WorkflowStep`. `STYLE.md` §2 says every
  wire type lives in `internal/protocol`; these twelve do not. §6.3 deals with this.

### 1.6 The gate

There is no `.github/` and no hosted CI. "CI" for this repository is two things:

- `just check` — `fmt-check vet staticcheck errcheck generate-check boundary test bench
  kb-check ui-test lines eval-check`;
- the three checks in `forge.toml` (`fmt` = `gofmt -l cmd internal`, `vet` = `go vet
  ./...`, `test` = `go test -race -count=1 ./...`), which Forge itself re-runs on every
  attempt in a fresh worktree.

Two properties of that gate matter to this plan. It is **path-globbed** — `fmt-check`,
`lines`, and `forge.toml`'s `fmt` all name the literal directories `cmd internal` — and
it is **package-pattern-globbed** — `vet`, `test`, `staticcheck`, `errcheck`, and `go
generate` all use `./...`. Section 2 turns on the difference between those two.

One generator exists: `internal/modes/generate.go` runs `go run
forge/internal/modes/gen` and produces `internal/modes/all/registry_gen.go`, checked by
`just generate-check`.

---

## 2. Mechanism: one module, four package trees

**Decision: keep one Go module. Express the four modules as four package subtrees under
`internal/` — `internal/core`, `internal/tools`, `internal/web`, `internal/tui` — and
enforce the dependency direction with an extended `just boundary`. Do not add `go.work`
now. Do not use git submodules at all.**

The layout is chosen so that promotion to a real multi-module workspace later is a
mechanical, additive step (§8). This is a decision about *when*, not *whether*.

### 2.1 Option A — Go multi-module workspace (`go.work` + a `go.mod` per module)

What it buys: the module graph is checked by the toolchain, so `core` cannot import
`web` even by accident (the `require` simply isn't there), and each module can carry its
own dependency set.

What it costs here, measured:

- **`./...` stops at the module boundary.** With a nested module and a `go.work`
  covering both, `go list ./...` from the repository root returns only the root
  module's packages. That is the expansion `go vet ./...`, `go test -race ./...`,
  `staticcheck ./...`, `errcheck ./...` and `go generate ./...` all use. Adopting
  Option A silently drops three of the four trees out of five checks in `just check`
  **and out of two of the three checks in `forge.toml`** until every one is rewritten
  as a per-module loop. A gate that fails open is worse than no gate.
- **Path-globbed recipes keep working; pattern-globbed ones do not.** `gofmt -l cmd
  internal` is path-based and would still cover a nested module. The inconsistency is
  the trap: formatting stays green while vetting quietly stops.
- **`GOWORK` is environment state, not repository state.** `just check-offline` runs
  the gate under `unshare -Urn` with a reduced `PATH`; Forge's own L1 re-runs
  `forge.toml`'s checks in a fresh worktree. A `go.work` that is gitignored (the usual
  advice) does not exist in that worktree; a committed `go.work` is one more
  every-branch-touches-it file, which `STYLE.md` §10 exists to avoid.
- **Refactoring gets expensive in exactly the place this migration is expensive.**
  Moving a type across a module boundary is a `replace`-directive and version-bump
  exercise; inside one module it is `git mv` plus `goimports`. §1.5's twelve
  store-as-DTO types are going to move more than once.
- **The benefit is mostly already bought.** An import cycle between *packages* is
  already a compile error. What Option A adds is a cycle check between *module graphs*,
  which is not the failure mode here. The failure mode is "a handler reaches into the
  engine's internals", and `just boundary` already proves that class of rule with `go
  list -deps` — three rules today, about twenty lines for the full four-way DAG.
- **One binary cannot spend the benefit.** Independent versioning and independent
  dependency sets pay off when parts ship separately. `cmd/forge` links all four trees
  into one executable; the linker already drops what a given subcommand doesn't reach.

### 2.2 Option B — nested git submodules

Rejected outright. Submodules break the atomic commit (`STYLE.md` §12's review gate
reviews one diff), break `go test ./...`, break `just check` as a single command, and
break Forge's own integrator, which rebases one branch of one repository onto `main`
(`DESIGN.md` §20). The repository's own automation would stop working on the repository.

### 2.3 Option C — package trees in one module, enforced by a linter (chosen)

- Every recipe in `Justfile` and every check in `forge.toml` keeps working **unchanged**,
  because everything stays under `cmd` and `internal`.
- `go test ./...` keeps covering the whole tree, which matters most during a migration
  whose main risk is 7,416 lines of internal tests.
- The DAG is enforced, not merely documented — by the mechanism the repository already
  uses and already trusts (§7).
- Cross-tree refactors stay cheap while the boundaries are still being discovered.
- The four trees are laid out so §8's promotion is additive: four `go.mod` files, one
  `go.work`, and the recipe rewrites — no import path changes.

The cost is honest: the compiler does not enforce the direction, one linter recipe
does. §7 makes that recipe fail closed, which is the property it lacks today.

---

## 3. Boundaries and the dependency direction

```
                         cmd/forge          (composition root; imports anything)
                        /    |    \
                       /     |     \
                    tui     web ──▶ tools
                       \     |      /
                        \    |     /
                         ▶  core  ◀
```

Four rules, and they are the whole contract:

| module | may import | must never import |
| --- | --- | --- |
| `internal/core` | stdlib, third-party, other `core` tiers (§3.1) | `tools`, `web`, `tui`, `cmd` |
| `internal/tools` | `core` | `web`, `tui`, `cmd` |
| `internal/web` | `core`, `tools` | `tui`, `cmd` |
| `internal/tui` | `core` | `web`, `tools`, `cmd` |
| `cmd/forge` | all four | — |

`tui` must not import `web`: the terminal client talks to the daemon over HTTP on the
Unix socket, so its dependency is the *wire* (`core/protocol`) and the *socket path*
(`core/daemon`), never the server implementation. That rule is what keeps a future
terminal UI honest — if it needs something, the daemon must expose it on the API, which
is precisely `DESIGN.md` §1's "every `forge` command is a thin client".

`web` may import `tools` because the tool routes (`GET /api/v1/tools`, `POST
/api/v1/tools/{name}`) dispatch into the registry, and `Server` holds a
`*tools.Registry`. `tools` must never import `web` — today it doesn't, and
`tools.Deps`/`tools.Request` are deliberately plain structs (store handle, clock,
logger, kb dir) with no HTTP in them. Keep it that way.

### 3.1 What `core` exports, and how it is layered inside

`core` is large — roughly 28,000 non-test lines — because Forge is a daemon. It is not
a dumping ground: it is four tiers, each importing only tiers below it.

| tier | packages | rule |
| --- | --- | --- |
| 0 — kernel | `core/model`, `core/protocol`, `core/logging` | `model` imports no Forge package; `protocol` imports only `model`; `logging` imports none. (Two of these are already linted.) |
| 1 — state & execution | `core/store`, `core/kb`, `core/worker`, `core/plugin` | `worker` may import tier 0 only — never `store` (one SQLite writer). `plugin` is the manifest contract, nothing else. |
| 2 — services | `core/stats`, `core/doctor`, `core/eval`, `core/integrator`, `core/modes` | tiers 0–1 only |
| 3 — policy & lifecycle | `core/engine`, `core/config`, `core/daemon` | tiers 0–2 only |

`core`'s exported surface, stated as the thing other modules are allowed to depend on:

- **the domain** — `model` (ids, states, transitions, branch names) and `protocol`
  (every wire type);
- **persistence** — `store` (every SQL statement, per `STYLE.md` §2) and `kb`;
- **the decisions** — the pure functions `budget.Decide`, `queue.Order`,
  `scheduler.Pick`, `router.Route`, `worker.DecideCleanup`, plus `engine`'s claim,
  apply, facts, prompt-assembly and auto-eval operations;
- **the execution engine** — `worker` (worktrees, manifests, supervisor, parser,
  reconcile, sandbox);
- **the process contracts** — `daemon` (lock, `daemon.json`, socket/DB/token file
  names, the restart env keys) and `plugin` (manifest, capabilities, scopes);
- **configuration** — `config` (`config.toml`, model table, routing policy);
- **the services** — `stats`, `doctor`, `eval`, `integrator`, `modes`, `logging`.

`core` exports **no** `http.Handler`, no `html/template`, no terminal rendering, and no
`tools.Tool` implementation.

---

## 4. Package map: every top-level package to its target

| today | non-test / test | target | note |
| --- | ---: | --- | --- |
| `internal/model` | 593 / 244 | `internal/core/model` | tier 0; boundary rule preserved verbatim |
| `internal/protocol` | 439 / 0 | `internal/core/protocol` | tier 0; boundary rule preserved |
| `internal/logging` | 785 / 458 | `internal/core/logging` | tier 0; leaf today, stays a leaf |
| `internal/store` | 5,237 / 2,318 | `internal/core/store` | tier 1; migrations move with it |
| `internal/kb` | 443 / 187 | `internal/core/kb` | tier 1; leaf |
| `internal/worker` | 6,366 / 3,147 | `internal/core/worker` | tier 1; the `worker ↛ store` rule moves with it |
| `internal/plugin` | 603 / 402 | `internal/core/plugin` | tier 1; manifest + capability/scope vocabulary only |
| `internal/stats` | 647 / 565 | `internal/core/stats` | tier 2 |
| `internal/doctor` | 446 / 331 | `internal/core/doctor` | tier 2 |
| `internal/eval` | 547 / 174 | `internal/core/eval` | tier 2; imports no Forge package today |
| `internal/integrator` | 600 / 449 | `internal/core/integrator` | tier 2 |
| `internal/modes` (+17) | 1,248 / 345 | `internal/core/modes` | tier 2; `//go:generate` path changes (§9.5); `modes/schema → worker` edge is the only modes→exec edge |
| `internal/tools` | 1,193 / 821 | `internal/tools` | becomes the `tools` module root; already imports only `core` packages |
| `internal/mcpserve` | 910 / 487 | `internal/tools/mcpserve` | the stdio MCP server `forge mcp` runs; imports `worker`, `protocol`, `logging` (all `core`) |
| `internal/controlplane` | 12,830 / 7,416 | split four ways | §5 |
| `cmd/forge` | 7,801 / 2,308 | split two ways | §6 |
| `plugins/*` | — | unchanged | `main` packages in this module; import nothing from `forge/internal` |
| `ui/`, `evals/`, `scripts/`, `testdata/`, `bench/` | — | unchanged | `ui/` is the Playwright harness against `web` |

## 5. Splitting `internal/controlplane`

`controlplane` becomes five destinations. The rule that decides each file: **does it
mention `*http.Request` or `html/template`?** If yes it is `web`; if it is the MCP
stdio client it is `tools`; otherwise it is `core`.

**→ `internal/core/engine`** (the decisions and the orchestration, 3,359 lines)

`budget.go` (469) · `apply.go` (472) · `config_models.go` is split off below ·
`facts.go` (364) · `scheduler.go` (312) · `prompt.go` (309) · `route_claim.go` (275) ·
`router.go` (234) · `autoeval.go` (227) · `queue.go` (189) · `ab.go` (153) ·
`prune.go` (136) · `handlers_plan.go` (133 — misnamed; `planFollowUps` takes a
`*store.Tx`, not a request) · `sweeper.go` (86)

**→ `internal/core/config`** (656 lines): `config.go` (284), `config_models.go` (372).
`config_models.ResolveModel` is the alias→id seam `ServerOptions.ResolveModel` consumes
— a `func` value, which is what keeps `web → core` one-directional.

**→ `internal/core/daemon`** (354 lines + part of `drain.go`): `daemon.go` (241 — the
lock, `daemon.json`, `LockFile`/`StateFile`/`SocketFile`/`TokenFile`/`DBFile`, the
`FORGE_SOCK_FD`/`FORGE_HTTP_FD`/`FORGE_RESTARTED` keys, `ListenerFromFD`),
`bootstrap.go` (113), and the non-HTTP half of `drain.go` (`waitInflightIdle`,
`markStateDraining`).

**→ `internal/tools/pluginbridge`** (369 lines): `plugintools.go`. It bridges a
`tools`-capability plugin's MCP stdio into the registry and imports exactly one Forge
package — `internal/tools`. It moves with zero edits beyond its import block. (Its two
`Server` mentions are in comments.)

**→ `internal/web`** (everything else, ~8,000 lines): `server.go` (736, minus the engine
fields), `handlers_operator.go` (1,027), `handlers_worker.go` (948),
`handlers_repos.go` (604), `handlers_plugins.go` (500), `handlers_verify.go` (447),
`handlers_health.go` (434), `handlers_workflows.go` (300), `handlers_lineage.go` (294),
`handlers_proposals.go` (251), `handlers_assistant.go` (215), `handlers_tools.go` (178),
`handlers_kb.go` (109), `handlers_usage.go` (93), `handlers_doctor.go` (92),
`handlers_timeline.go` (89), `handlers_stats.go` (88), `stream.go` (232), `rpc.go` (102),
`routine_templates.go` (74), the drain handler, and `internal/web/ui/` — `ui.go` (861),
`ui_kb.go` (207), `ui_actions.go` (83) and the 20 `.html` templates plus `static/`. The
`//go:embed ui/*.html ui/static/*` directive is directory-relative, so it travels with
the files unchanged.

The type split that makes this possible:

```go
// internal/core/engine
type Engine struct {
    store  *store.Store
    policy Policy
    cfg    *config.Config
    clock  func() time.Time
    log    *slog.Logger
    // the injected seams: RegisterRepo, AddRepo, StartApp, ExecRestart,
    // PluginStart, ModelCall, EvalFn, … — func values, never interfaces
    // that could carry a web type back across the boundary.
}

// internal/web
type Server struct {
    eng *engine.Engine
    ui  *ui.UI
    mux *http.ServeMux
    // transport, auth, drain state, inflight counter — nothing else.
}
```

`ui.UI` keeps its `*store.Store` (§1.4): the UI is a read path over `core/store`, which
is a legal `web → core` edge. It gains nothing from going through `Engine` and would
lose a great deal of directness.

## 6. Splitting `cmd/forge`

`cmd/forge` is two programs sharing a directory: a thin HTTP client, and the process
that hosts the daemon. Split on that line.

**→ `internal/tui`** — the client half. `cli_client.go` (388) becomes
`internal/tui/client`; its only `core` dependencies are `daemon.SocketFile` and
`protocol`. The operator-facing command files follow: `cmd_task.go` (668),
`cmd_plugin.go` (546), `cmd_routine.go` (284), `cmd_kb.go` (265), `cmd_repo.go` (261),
`cmd_workflow.go` (257), `cmd_task_logs.go` (202), `cmd_proposal.go` (195),
`cmd_stats.go` (149), `cmd_queue.go` (143), `cmd_usage.go` (109), `cmd_eval.go` (107),
`cmd_backup.go` (99), `cmd_doctor.go` (153), `cmd_service.go` (184), `cmd_cleanup.go`
(54), `cmd_prune.go` (51), `cmd_retro.go` (45), `cmd_init.go` (349). Roughly 4,500
non-test lines. A future terminal UI is a new package under `internal/tui` next to
these, reusing `client`.

Not all nineteen are *only* HTTP clients: `cmd_init.go` runs interactive setup against
`worker` and `doctor`, `cmd_cleanup.go` and `cmd_task_logs.go` call `worker`,
`cmd_eval.go` runs `eval` locally, `cmd_doctor.go` calls `doctor`, `cmd_service.go`
writes systemd units, and eight of them decode `store` row types (§6.3). Every one of
those is a legal `tui → core` edge; none is a `tui → web` edge, which is the rule that
matters. `cmd_service.go` imports no Forge package at all.

**stays in `cmd/forge`** — the composition root and the non-client hosts, per
`STYLE.md` §1 ("registries are values … constructed in `cmd/forge`") and §3 ("construct
things in `main` and pass them down"): `main.go` (212, the dispatch table),
`cmd_daemon.go` (1,038 — the only `store.Open`), `cmd_daemon_restart.go` (195),
`cmd_daemon_resilience.go` (204), `cmd_worker.go` (79), `cmd_mcp.go` (80),
`run_supervisor.go` (460), `fake_claude.go` (630), `plugin_omarchy.go` (394).

`cmd/forge`'s 44 `controlplane` references re-point mechanically:

| identifier group | new home |
| --- | --- |
| `SocketFile`, `DBFile`, `LockFile`, `TokenFile`, `ReadState`/`WriteState`, `TryLock`/`Lock`/`IsLocked`/`LockFromFD`/`ErrLocked`, `ListenSocket`/`ListenTCP`/`ListenerFromFD`/`WaitForSocket`, `EnvSockFD`/`EnvHTTPFD`/`EnvRestarted`, `ProcStart`, `Bootstrap`/`BootstrapOptions`, `ReadToken` | `core/daemon` |
| `Config`, `LoadConfig`, `WriteDefaultConfig` | `core/config` |
| `Prune`/`PruneInput`/`PruneBackups`, `WriteBackupArchive`/`UnpackBackup`/`BackupInputs`/`LatestBackup`, `NewBudgetPolicy`, `Usage`/`WindowUsage` | `core/engine` |
| `NewServer`/`ServerOptions`, `NewUI` | `web` |
| `NewPluginTools`/`PluginTools`/`RegisterPluginTools` | `tools/pluginbridge` |

**6.3 The DTO exception.** §1.5's twelve `store` row types are decoded by `tui`
commands from API responses. Two ways out:

- *Promote* them into `core/protocol` as response types and have `web` marshal those —
  correct per `STYLE.md` §2, but it is a wire-format change touching `web`, `tui`, the
  browser UI's JSON consumers and their tests.
- *Except* them — `tui` may import `core/store` **for types only**, recorded as one
  named exception in the boundary linter.

**Take the exception during the migration; promote afterwards as its own task.** Mixing
a wire-format change into a move-only refactor is how a migration stops being
verifiable. The exception is written into the linter as a single explicit line so it
cannot spread silently.

---

## 7. Enforcement: `just boundary`, made to fail closed

`just boundary` today has a defect that this migration would walk straight into. Its
rules are shell loops of the form:

```
@for p in $(go list ./internal/worker/... 2>/dev/null); do … done
```

If the path stops existing — which is exactly what `git mv internal/worker
internal/core/worker` does — `go list` errors into `/dev/null`, the loop body never
runs, and the recipe reaches its final `@echo "boundary: ok"`. **The linter passes
because the rule evaporated.** Fix that before moving anything: a rule whose package
set is empty must be a failure.

The extended recipe expresses §3's table as data:

```
# tree                  may reach                                   (everything else is a violation)
core                    core
tools                   core tools
web                     core tools web
tui                     core tui                                    # + one exception: core/store types (§6.3)
```

plus the three existing rules, which survive verbatim with their paths updated:
`core/worker ↛ core/store`, `core/model ↛ any Forge package`, `core/protocol ↛
anything but core/model`. Each is one `go list -deps` per package and a `grep` — the
idiom already in the file, and the note already in the recipe's comment about not using
`grep -q` (a SIGPIPE must not turn a violation into a pass under `pipefail`) applies
unchanged.

---

## 8. Migration sequence

Each stage is a separate commit (or a short series of them) and leaves `just check`
green. Stages 1 and 3 are the only large diffs; both are decomposed so no single commit
is unreviewable.

**Stage 0 — enforce before moving.** Fix §7's fail-open defect and add the four-way rule
table to `just boundary`, with the rules describing *today's* graph (`controlplane` as
`web`, everything else as `core`). Nothing moves; the recipe is green and now actually
proves something. Land this document.

**Stage 1 — lift the leaves into `core`.** `git mv internal/X internal/core/X` for
`model`, `protocol`, `logging`, `kb`, `store`, `worker`, `plugin`, `stats`, `doctor`,
`eval`, `integrator`, `modes`. One commit per package, `git mv` (not delete + add) so
rename detection keeps the diffs readable and merges survivable. In the `modes` commit,
update `//go:generate go run forge/internal/modes/gen` and re-run `go generate ./...`,
committing `registry_gen.go` in the same commit. In the `worker`, `model`, `protocol`
commits, update the corresponding boundary rule paths. Update
`scripts/smoke-m9.sh:225–227` and `scripts/smoke-m11.sh:102`, which name `internal/`
paths. `gofmt -l cmd internal`, `go vet ./...`, `go test ./...` all keep their coverage
because nothing left `internal/`.

**Stage 2 — split the type, keep the package.** Introduce `Engine` inside `package
controlplane`. Move `store`, `policy`, config and the 20 `func` seams off `Server` onto
it; `Server` keeps `eng *Engine` plus mux/auth/drain/inflight. Reclassify each of the
193 `Server` methods: anything that never takes an `*http.Request` becomes an `Engine`
method. **All 49 test files keep compiling untouched**, because the package boundary has
not moved — this is the whole reason Stage 2 precedes Stage 3. Guard the split with a
throwaway check: no file defining an `Engine` method may import `net/http`.

**Stage 3 — cut `core/engine`, `core/config`, `core/daemon` out.** Move file *pairs*
(`budget.go` + `budget_test.go`, then `queue.go` + `queue_test.go`, …), commit and run
`just check` after each pair. Start with the ten files that mention `*Server` zero
times (§1.3) — `budget` (469 + 442 lines of tests) is the cleanest first cut. Each pair
may require exporting identifiers the remaining `web` code still uses; the pure decision
functions (`Decide`, `Order`, `Pick`, `Route`) are already exported by `STYLE.md` §2's
rule, so the churn is smaller than the line counts suggest. Move `handlers_plan.go`
here despite its name.

**Stage 4 — form `tools`.** `git mv internal/mcpserve internal/tools/mcpserve` and
`internal/controlplane/plugintools.go` → `internal/tools/pluginbridge/`. Tighten the
linter: `tools` may reach `core` and `tools` only.

**Stage 5 — rename the remainder to `web`.** `git mv internal/controlplane
internal/web`, package `controlplane` → `web`, and move `ui*.go` + `ui/` into
`internal/web/ui` as its own package (`ui.UI` is already a self-contained type with its
own mux). Re-point `cmd/forge`'s 44 identifiers per §6's table. `just lines`' `find
internal -path '*/ui/*'` still matches. Tighten the linter: `web` may reach `core`,
`tools`, `web`.

**Stage 6 — form `tui`.** Move `cli_client.go` and the 19 client command files into
`internal/tui`, leaving `cmd/forge` as dispatch + the daemon/worker/MCP hosts. Record
§6.3's store-types exception in the linter as one explicit line. Tighten the linter:
`tui` may reach `core` and `tui`.

**Stage 7 — close the loop.** Remove the transitional allowances, confirm the linter's
rule table is exactly §3's table, and update the package names in `DESIGN.md` §1's
diagram, `STYLE.md` §1 (`SchedulerPolicy`, `Store` "in `controlplane`") and `STYLE.md`
§2 (`controlplane/budget.Decide`, `controlplane/queue.Order`).

**Stage 8 — optional, not scheduled: promote to `go.work`.** Verified feasible in
place: a nested `go.mod` under `internal/` *is* importable from the root module (the
`internal` rule is satisfied because the importer is inside the tree rooted at the
parent of `internal`), so promotion needs no import-path changes — four `go.mod` files,
one `go.work`, four `require`/`replace` pairs. The price is §2.1's: rewrite `vet`,
`test`, `staticcheck`, `errcheck`, `generate-check` in the `Justfile` and `vet` + `test`
in `forge.toml` as per-module loops, in the same commit, or the gate silently narrows to
one tree. Do this only when a second consumer of one of the trees actually exists.

---

## 9. Risks

**9.1 The boundary linter fails open.** §7. This is the highest-severity risk in the
plan because it makes every other risk invisible. Stage 0 exists solely to fix it.

**9.2 Import cycles.** Two candidates, both already managed and both easy to break:

- *engine ↔ web* through the injected seams. `ServerOptions`' 39 fields include 20 `func`
  values that `cmd/forge` supplies — worker-side operations (`AddRepo`, `StartApp`,
  `RegisterRepo`) handed *into* the daemon. That shape is precisely what keeps the graph
  acyclic. The rule: seams stay `func` types. The moment one becomes an interface that a
  `web` type satisfies and `core` names, the cycle is one commit away.
- *tools ↔ core/engine*. `tools.Deps` holds a concrete `*store.Store` and
  `tools.Registry` is a field on `Server`. Today `tools` imports only `kb`, `model`,
  `protocol`, `stats`, `store` — no engine, no HTTP. `tools` must never import
  `core/engine`; if a tool needs a decision, the decision moves down into `core`, not
  the tool up.

**9.3 The plugin wire contract.** Three surfaces must not shift: `plugin.Manifest`
(the `plugin.toml` fields), the capability (`events`, `tools`, `intake`, `annotate`) and
scope (`events:read`, `work:write`, `tools:provide`, …) vocabularies, and the
newline-delimited JSON-RPC 2.0 stdio framing shared by `pluginbridge` and
`tools/mcpserve`. Moving `internal/plugin` → `internal/core/plugin` changes no on-disk
or on-wire shape — the manifest is TOML, the tokens are strings, the framing is bytes.
The invariant that makes this safe is that the four first-party plugins import nothing
from `forge/internal`; they are `main` packages that speak HTTP and stdio. **Re-check
that invariant at Stage 1 and Stage 4**: the day a plugin imports `forge/internal/...`,
every move in this plan becomes a plugin-facing break, and `just build-plugins` becomes
part of the gate.

**9.4 The tests.** 49 internal test files, 7,416 lines, all `package controlplane`,
all reaching unexported identifiers, sharing helpers (`harness`, `homeServer`,
`newPluginServer`, `newApplyFixture`, …); eleven construct a `Server` directly. This is
the migration's real cost. Mitigations, in order of importance: Stage 2 splits the type
inside the package so no test moves; Stage 3 moves one file-pair at a time; shared
helpers move to the tree where most of their users land and are exported only where a
second tree genuinely needs them (an exported test helper in `core` used by `web` is
acceptable; a `web` test reaching into `core` internals is not).

**9.5 The generator.** `//go:generate go run forge/internal/modes/gen` is
module-path-qualified. Move it and regenerate in one commit, or `just generate-check`
(`go generate ./...` then `git status --porcelain -- '*_gen.go'`) fails.

**9.6 The gate's globs.** `forge.toml`'s `fmt` and the `Justfile`'s `fmt-check` and
`lines` name `cmd internal` literally. Everything in this plan stays under `internal/`,
so they need no edit — which is a feature, not an accident. Any variant that moves a
tree to the repository root must update all three in the same commit, or formatting
stops being checked without any check turning red.

**9.7 Merge conflicts with parallel Forge work.** Forge integrates concurrent agent
branches into this repository (`DESIGN.md` §20), and `STYLE.md` §10's entire conventions
section exists to minimise conflicts. A repository-wide import-path rewrite conflicts
with every in-flight branch. Run Stage 1 with the queue drained; keep each stage a small
commit; use `git mv` so rename detection and `mergiraf` have something to work with.

**9.8 Documentation drift.** `DESIGN.md` §1's diagram and `STYLE.md` §§1–2 name packages
by their current paths (`Executor`/`OutputParser` "in `worker`", `SchedulerPolicy`/
`Store` "in `controlplane`", `controlplane/budget.Decide`, `controlplane/queue.Order`).
Stage 7 updates them; until then the docs describe paths that no longer exist, which is
a review-gate failure waiting to happen.

**9.9 Smoke scripts.** `scripts/smoke-m9.sh:225–227` greps `internal/integrator/` and
runs `go test ./internal/integrator`; `scripts/smoke-m11.sh:102` names
`./internal/controlplane`. They are not in `just check`, so they rot silently. Update
them with their stage.

---

## 10. Non-goals

- **This does not build a TUI.** `internal/tui` starts as the CLI's client half. A
  terminal UI is a later feature that lands inside a boundary already drawn — which is
  the point of drawing it now, while `tui` is 4,500 lines of straightforward HTTP
  client rather than a rendering loop entangled with a server.
- **This does not change behaviour.** No HTTP route, no SQLite migration, no plugin
  manifest field, no subcommand. Every stage is a move, a rename, or a mechanical type
  split, and `go test -race ./...` is the proof.
- **This does not turn the browser UI into an API client.** §1.4: the UI reads the
  store directly through 24 methods. That is a rewrite with its own justification and
  its own risk, and it is not a prerequisite for any boundary here.
- **This does not split `worker` into a fifth module**, even though it is the cleanest
  candidate — it already satisfies the strictest boundary rule in the repository. The
  operator's four names are the target; `core/worker` stays a tier-1 package with its
  existing `↛ store` rule intact, and promoting it later costs one linter line.
