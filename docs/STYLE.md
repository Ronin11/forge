# Forge code standard

Every commit is reviewed against this document. A reviewer who cannot point at a rule
here has an opinion, not a finding; a rule that is wrong gets fixed here first.

The target reader is a meticulous forty-year veteran who finds clever code embarrassing.
Code should be obvious on first read, boring on second read, and still correct at 3 a.m.

## 1. Boundaries are interfaces; extension is registration

- `Executor`, `OutputParser`, `Tool`, `Mode`, `SchedulerPolicy`, `VerificationCheck`, and
  `Store` are interfaces. An interface with one consumer lives in that consumer's
  package (`Executor`, `OutputParser` in `worker`; `SchedulerPolicy`, `Store` in
  `controlplane`); one with several consumers lives in the package named for the
  concept, which imports only `model`/`protocol` (`Mode` in `modes`, `Tool` in
  `tools`, `VerificationCheck` in `verify`). Next to each is a registry type with
  `Register(name, impl)` and `Lookup(name)`.
- Adding an implementation is **one new file (or one new package, for a mode with its
  embedded prompt and schema) plus one `Register` call** at the wiring point
  (`cmd/forge`). Nothing else changes. If adding one requires editing a switch, the
  design is wrong.
- Core packages never type-switch on concrete implementations. If a caller needs to
  know a capability, the interface exposes it (`Capabilities() []string`), not the type.
- Registries are values passed explicitly (constructed in `cmd/forge`, handed to
  `worker.New`, `controlplane.New`). There is no package-level registry.

## 2. DRY at the level of concepts, not lines

- Two similar functions are fine. Two copies of a **rule** are a bug. Rules that have
  exactly one home:
  - Target state transitions → `model.Transition`.
  - Work state derivation → `model.DeriveWorkState`.
  - ID and short-ID formats → `model.NewID`, `model.ShortID`.
  - Branch naming → `model.BranchName`.
  - The cleanup / retain decision → `worker.DecideCleanup` (pure function).
  - The budget admission decision → `controlplane/budget.Decide` (pure function).
  - Queue ordering → `controlplane/queue.Order` (pure function).
  - Every SQL statement → `internal/store`.
  - Every wire type → `internal/protocol`.
- Prefer a pure function over a method when the decision depends only on its inputs.
  Pure decision functions are the ones tested hardest.
- Do not abstract two things that merely look alike today. Wait until the third.

## 3. Boring Go

- **No globals.** No package-level mutable state, no `init()`, no `sync.Once` singletons.
  Construct things in `main` (or `cmd/forge`) and pass them down. The single
  exception is `main.version`, which the linker sets with `-X`.
- **No reflection** except `encoding/json`. No `unsafe`. No code generation.
- **Generics** only when the third copy of something would otherwise appear (§2's
  rule), e.g. `Registry[T]`. A generic with one instantiation is a finding.
- **`context.Context`** is the first parameter of every function that blocks, does I/O,
  spawns a process, or may be cancelled. Never stored in a struct. Carve-out: a
  function whose only I/O is a few non-blocking syscalls (`open`, `stat`, `rename`,
  `flock` with `LOCK_NB`) takes no context — a context it could not honour is a lie.
- **Errors** are wrapped with what was being attempted, in the caller's vocabulary:
  `fmt.Errorf("resolve base for %s: %w", repo, err)`. Never `errors.New(err.Error())`.
  Never discarded — the tree is `errcheck`-clean, including `defer f.Close()` patterns
  (use a named return or an explicit `if err := f.Close(); err != nil`). The only
  exclusions are the three `fmt.Fprint*` functions listed in `.errcheck_excludes`,
  for CLI output; nothing is added to that file without a note here. Sentinel errors
  are exported `var ErrX = errors.New("…")` and tested with `errors.Is`.
- **Time.** Wall-clock timestamps are `time.Time` in UTC. Durations that will be
  compared or summed come from the monotonic clock (`time.Since(start)` on a `start`
  captured with `time.Now()`); they are stored as `int64` microseconds with a `_us`
  suffix. Never subtract two wall timestamps to get a duration — with one carve-out:
  an interval that spans processes or comes from external timestamps (`queue_wait`,
  `wait_human_us`, the budget window arithmetic) is necessarily wall-clock; such
  values are flagged (`attrs.clock = "wall"`, a doc comment on the column) and never
  summed with monotonic ones. Anything that needs the current time takes a
  `func() time.Time` (a `Clock`) so tests can control it.
- **Randomness** comes from `crypto/rand` for IDs and tokens, `math/rand/v2` with an
  injected source for jitter.
- **Concurrency.** Every goroutine has an owner that waits for it (`errgroup` or a
  `sync.WaitGroup`) and a way to be stopped (context). No fire-and-forget. Shared state
  is guarded by a mutex declared directly above the fields it guards, with a comment
  naming them.
- **Doc comments** on every exported symbol say *why* it exists or what invariant it
  holds, not just what it does. `// NewID returns a new ID.` is a finding.
- **Naming.** Short names for short scopes; full words for fields and exported symbols.
  No `Manager`, `Helper`, `Util`, `Common`. A type is named for what it *is*
  (`Supervisor`, `Lease`, `Manifest`), a function for what it *does*.
- **Files** are ≤ ~600 lines and named for the concept they hold (`lease.go`,
  `cleanup.go`), never `utils.go`, `misc.go`, `helpers.go`.
- **Packages** own a concept. `internal/model` (state machines, IDs, pure rules);
  `internal/protocol` (wire types only, no logic beyond validation); `internal/store`
  (SQLite; all SQL lives here); `internal/controlplane` (http, scheduler, budget, queue,
  ui); `internal/worker` (config, git, worktree, manifest, executor, parser, supervisor,
  reconcile); `internal/tools`; `internal/modes`; `internal/verify`; `internal/kb`.
  The worker package never imports controlplane — `just boundary` proves it.
  `model` and `protocol` import nothing from Forge.
- **Configuration** is parsed once into a struct with defaults applied and validated
  at load time; the rest of the program never sees a raw map.
- **Logging** follows §8: `log/slog` through `internal/logging` only, structured, with
  IDs and sizes, never prompt bodies or tool output. Errors are logged once, at the
  top of the stack that handles them.
- **Third-party dependencies** need a reason recorded in `NOTES.md`. Current allow-list:
  `modernc.org/sqlite` (pure-Go SQLite, no cgo), `github.com/BurntSushi/toml`,
  `github.com/robfig/cron/v3` (parsing only), `github.com/mark3labs/mcp-go` (MCP
  protocol; if it disappoints, the stdio JSON-RPC surface Forge needs is small enough
  to write by hand), `golang.org/x/sync` (errgroup).

## 4. Performance where it matters

- Event ingestion is batched: the worker flushes every 500 ms or 100 events or 256 KiB,
  whichever first; the control plane inserts a batch in one transaction with a
  prepared statement.
- SQLite runs in WAL mode with `synchronous=NORMAL`, `busy_timeout=5000`, foreign keys
  on, one writer connection (serialised in Go) and a small read pool.
- Every multi-statement write is an explicit transaction. Every hot statement is
  prepared once and reused.
- Percentiles are computed in Go over rows fetched by an indexed query, never with
  window functions on the whole table.
- List endpoints fetch children with one `IN (...)` query per child type, never one
  query per row.
- `just check` runs benchmarks for the event path and the stats query and fails if
  either regresses past its recorded threshold (`bench/threshold.txt`).
- Do not optimise anything else without a measurement in `NOTES.md`.

## 5. Reliability is tested by breaking things

- Tests use real temporary Git repositories (`git init` in `t.TempDir()`), real SQLite
  files, and real child processes. Never a mock of `git`. A shell script standing in
  for `claude` (a fake *executor*) is not a mock — it is a real child process that
  emits a recorded stream — and is how the worker is tested without spending budget.
- The required breakage suite: kill the worker mid-attempt; kill the control plane
  mid-claim; corrupt a manifest; expire a lease; run two workers on the same checkout;
  a parser given garbage and truncated lines; a worktree removed underneath the worker.
- Table-driven tests for every pure rule (transitions, cleanup decision, budget
  decision, queue order). Every state-machine edge, allowed and refused, is listed.
- `-race` always. Tests are deterministic: injected clocks, no `time.Sleep` for
  synchronisation (use channels or polling with a deadline helper).
- A bug fix comes with the test that would have caught it.

## 6. Data and IDs

- IDs are 32 lower-case hex characters from `crypto/rand`; short IDs are the first 8.
  Every table's primary key is the ID as `TEXT`. Foreign keys are declared.
- Every row that records a fact about an attempt is immutable once the attempt is
  terminal. Facts are computed once, in one function, and inserted once.
- JSON columns hold small, bounded structures (`tool_calls_by_name`, `attrs`), never
  raw output. Raw output lives in files under `data_dir/output/`.
- Migrations are numbered SQL files embedded in `internal/store`, applied in order, in
  a transaction, recorded in `schema_migrations`. No down-migrations.

## 7. HTTP and CLI

- The API is `/api/v1/…`, JSON, on the Unix socket and the loopback listener with the
  auth rules of `DESIGN.md` §1.1. Errors are `{"error": "…"}` with an appropriate
  status. Handlers validate, call one store or service method, encode the result —
  no business logic in handlers.
- List endpoints are bounded (`limit`, default 50, max 500) and stable-ordered.
- The CLI uses the standard `flag` package with one file per subcommand under
  `cmd/forge`. Every command has `-h` text and an exit code of 0/1/2 (ok / failed /
  usage).
- The UI is `html/template` + one stylesheet + vanilla JS. No framework, no bundler,
  no inline event handlers; scripts attach behaviour by `data-` attributes.

## 8. Logging

Logs are for debugging. The `journal` table (§10) is the audit trail; a log line is
never evidence of what happened and nothing reads logs to decide anything.

- **One mechanism.** `log/slog` via `internal/logging`. No `log.Printf`, no
  `fmt.Fprintln(os.Stderr, …)` outside CLI output, no `slog.Default()`. Every package
  gets its logger from `handler.For("<component>")` — dotted names (`store`,
  `worker.git`, `controlplane.http`, `plugin.<name>`) — passed in at construction,
  never fetched from a global. The `component` attribute is on every line.
- **Levels:** `trace` (a custom level below debug: SQL statements, HTTP bodies, raw
  executor lines), `debug` (state changes, decisions with their inputs), `info` (what
  an operator watching would want: start/stop, claim, complete, retained), `warn`
  (degraded but continuing: a failed fetch, a retry), `error` (something that needed
  a human or lost work). Per-component levels: `--log-level store=trace,worker=debug`.
- **Flags on every subcommand:** `--log-level`, `--log-format text|json`, `-v`
  (debug), `-vv` (trace), with `FORGE_LOG_LEVEL` / `FORGE_LOG_FORMAT` and the `[log]`
  config section; precedence **flag > env > config > default**. A subcommand builds
  its `FlagSet` through `cmdContext.flags`, which registers them.
- **Two sinks.** The flags control stderr only. The daemon and the worker also
  always write JSON at `debug` to `<forge home>/logs/<component>.log` (`[log] dir`
  overrides the directory; size rotation keeps `max_files` rotated generations
  besides the live file, rotating when it would exceed `max_size_mb`); that sink
  ignores the flags. The level spec that wins precedence replaces the others whole
  — a `--log-level store=trace` does not inherit an env default. `-v`/`-vv` only
  ever raise verbosity. SIGUSR1 raises the default to at least debug (or restores
  it) and leaves component overrides alone. Records are flat: `WithGroup` is a
  no-op so correlation fields stay top-level.
- **Correlation travels in `context.Context`** (`logging.ContextWith`) and is stamped
  by the handler: `request_id` on every HTTP request, `attempt_id`/`target_id`/
  `work_id` inside an attempt, `plugin` inside a plugin, `span_id` inside a span.
  Code inside an attempt logs with the `*Context` methods and the attempt's context;
  **a log line inside an attempt without `attempt_id` is a finding**, as is a bare
  `logger.Info(...)` where a context is in scope. Tests may assert with
  `logging.HasAttr`.
- **Runtime changes.** `SetLevels` via the API (`forge daemon log-level X`) and
  SIGUSR1 toggling debug. A Forge process that starts another Forge process passes
  `logging.Environ` so the child inherits level and format, and captures the
  child's stderr with `logging.Forward` under the child's component.
- **Executor output is not log.** stream-json and executor stderr go to the attempt
  output file and the parser; they are mirrored to the log only at `trace`, one
  line per line, bounded.
- **Never log** prompt bodies, tool output, tokens, or file contents. Log IDs,
  names, sizes, durations, and decisions.

## 9. Journal

Every state change of a Work, Target, Attempt, Question, or Proposal — and every
daemon lifecycle event and plugin decision (`entity_type` `daemon`, `plugin`) —
writes one row to `journal(id, ts, kind, entity_type, entity_id, payload)` **in the
same transaction** as the change, through the one store helper every state-changing
method calls. `id` is the monotonic order of events in the system; `payload` holds
the transition (`from`, `to`, `reason`, and the actor). The journal is the audit
trail and the input to "what happened to X"; it is never reconstructed from logs. A
store method that changes state without a journal row is a finding, and the store
tests assert the row for every transition.

## 10. Review gate

Before each commit the author spawns a reviewer with this document and the diff. The
reviewer reports findings as `file:line — rule § — problem`. Every finding is fixed or
recorded in `NOTES.md` with a reason. `just check` (fmt, vet, staticcheck, errcheck,
`-race` tests, benchmarks, boundary, `forge kb check`, browser tests) is green at every
milestone boundary.

Commit messages are Conventional Commits (`feat(worker): …`, `fix(store): …`,
`docs: …`, `test: …`, `chore: …`); the body says why.
