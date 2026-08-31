# Forge build notes

Running log of decisions, cuts, review findings not fixed, and open questions.
Newest entries at the bottom of each section. Dates are absolute.

## Decisions

- **2026-08-30 · V2 starts on top of V1 history.** The repo carried five V1 commits
  whose files had been deleted from the working tree. The deletions were committed
  (`chore: clear V1 tree`) rather than rewriting history; V1 stays readable with
  `git show 0148b45:…` for reference. Nothing from V1 is reused without re-reading it
  against `docs/STYLE.md`.
- **Module path is `forge`.** Not published; a dotted path would be ceremony.
- **Std `flag`, one file per subcommand.** Sixteen subcommands do not justify cobra.
- **IDs are 32-hex; short IDs 8; branch `forge/<slug>-<attempt8>`; worktrees are a
  flat `<data_dir>/worktrees/<attempt-id>`.** Flat paths make the manifest's
  "worktree path must equal the owned path" check a string equality.
- **Fetch is best-effort** (`DESIGN.md` §5.2). Factory required the remote's truth
  because it ran a fleet; Forge runs on one laptop that is often offline. Every
  attempt records whether the fetch succeeded, so an analysis can exclude stale bases.
- **Process identity is `/proc/<pid>/stat` start time**, Linux only. Factory used
  `ps -o lstart= -o command=`; the spec fixes the platform to Arch Linux so the
  simpler exact check wins. A second platform is one file.
- **One Attempt per Target across resumes.** A `waiting_human` pause keeps the same
  attempt, worktree, branch, and session; `launches` counts processes; elapsed time
  carries over through the manifest so `elapsed_us` is monotone and excludes waiting.
- **Claim is admission.** The budget decision is evaluated at claim time by one pure
  function; there is no separate "admitted" state to keep consistent.
- **Burn-down line is `target × fraction-of-window-elapsed`.** Backlog admits when
  utilisation is below the line; the line rises to the target at reset. Simple to
  explain, one formula, no tuning knobs beyond the targets.
- **`forge mcp` is a pure HTTP client of the control plane.** One SQLite writer;
  tool spans are recorded where the tool runs.
- **Reconcile does not call `complete` for a Target the control plane already
  closed** — it patches cleanup fields. A late `complete` for an open Target is a
  normal completion with `worker_restart` as the reason.
- **2026-08-30 · Review-gate decisions (M0).** `verifying` holds no lease; the sweeper
  covers only `claimed/preparing/running`. The heartbeat runs from claim until
  `complete` returns because L1 checks outlast a 30 s lease. Verify Work has no
  routine, so a `concurrency = 1` routine can still be verified. Events carry a
  `source` in their primary key so worker, `forge mcp`, and control-plane spans never
  collide, and the manifest carries `next_seq` so a resume never collides with the
  first launch. Repository tools (`forge_check`, `forge_repo_status`,
  `forge_diff_summary`) run inside `forge mcp` (a child of the agent, in the
  worktree, in its process group) rather than in the control plane. `forge_ask` is
  non-blocking; "open Question at exit ⇒ waiting_human" is the single rule.
  Auto-revert only ever restores the last human-approved generation and is part of
  the approved A/B plan — that is how it squares with constitution 5. `forge cleanup`
  is a worker-local command that takes the cross-process per-repository flock
  (`<data_dir>/locks/<repo>.lock`, shared with the running worker); `retained`
  manifests are final so the worker never races it. `forge prune` deletes rows via
  the API and files via `worker.toml` (both configs are on the one machine).
- **2026-08-30 · Logging standard and journal added before any store code.** One
  handler, per-component levels by longest dotted prefix, correlation attrs in
  context, stderr governed by flags, file sink fixed at debug JSON with size
  rotation written in-package (~80 lines; a dependency was not worth a reason).
  `-v`/`-vv` override the default level but keep `--log-level` component
  overrides. SIGUSR1 toggles debug and remembers the previous level; an explicit
  set clears that memory. The journal table is *specified* (`DESIGN.md` §3, `STYLE.md`
  §9) to be in migration 1 and written in the same transaction as every
  Work/Target/Attempt/Question/Proposal state change; the store lands in M1. Logs
  are never authoritative. `OpenFileSink` takes no context (STYLE §3 carve-out for
  non-blocking syscalls). Detached processes get `<component>.stdio.log` for raw
  stdio so the structured file never receives duplicate lines. The daemon/plugin work discussed
  separately (a `daemon` subcommand, plugin loading) is the M6/M7 delta; only the
  vocabulary (`daemon`, `plugin` component) is used here.
- **2026-08-30 · M6's process model is designed in before M1** (human decision:
  "fold M6 into the design now"). The daemon is the only SQLite opener; the worker
  is a detached child (or a systemd unit); the CLI is a thin client over a Unix
  socket with auto-start; `task` is the user-facing word. `forge run`/`forge
  serve`/`forge submit`/`forge answer`/`forge approve` never exist — the V2
  smoke steps are run with the M6 names (`docs/SMOKE.md`). M6 keeps only what
  M1–M5 do not need: drain-restart, `init`, `doctor`, `service`, capabilities,
  streaming (`logs -f`, `--wait`). Lease tolerance for a daemon restart is 120 s on
  both sides (daemon extends live leases on start; worker holds on through 120 s
  of failed heartbeats). Detached processes' raw stdio goes to
  `<component>.stdio.log`, structured logs to `<component>.log` — a deliberate
  split from the prompt's "stdio to worker.log" so the structured file never sees
  duplicate or unstructured lines. L3 sign-off gets `forge task approve|reject`,
  which the M6 tree lacked. `forge-m6-m7-prompt.md` stays the source for M7's
  plugin/indicator detail until `docs/PLUGINS.md` is written at M7 start.
- **2026-08-30 · Fold-in review decisions.** The CLI hands the daemon its locked
  `daemon.lock` descriptor as an extra file (no unlocked window); the daemon keeps
  lock and listener descriptors without `FD_CLOEXEC` across the drain `exec` (no
  socket gap); the daemon unlinks a stale socket itself after locking. A daemon
  start spawns a worker only if the worker data-dir lock is free; `daemon stop`
  stops the worker it spawned (`--keep-worker`). `--repo` resolves name → path →
  `<projects_root>/X` and registers unregistered checkouts on the fly by appending
  to `worker.toml`, which the worker re-reads every registration tick; the daemon
  re-reads `config.toml` only on restart (no SIGHUP mechanism). Lease extension on
  daemon start covers every claimed/preparing/running Target regardless of stored
  expiry. `complete` is idempotent by lease-token hash. `routine_name = "ad-hoc"`.
  Journal gains `daemon` and `plugin` entity types. Spawned processes get a fixed
  environment pass-through list (plugins included — a deliberate widening of
  "nothing else" so Python/Node plugins can start). `doctor` is one package used
  by the CLI (local checks, works with the daemon down) and `GET /api/v1/doctor`.
  `--wait` exits with the derived task state. systemd units use `KillMode=process`
  so the detached worker outlives a daemon restart. Schema-only handshake mismatch
  is still a mismatch, with a "to migrate" hint. Plain `daemon restart` (no drain)
  is M1 so M3 smoke 17 can restart; drain is M6.
- **2026-08-30 · M8–M12 folded into the design the same way as M6** (schema, state
  machine, config shapes, test harness; behaviour stays in its milestone). From M1:
  `fake-claude` is the test executor and `just check` runs offline (the M8 rule,
  adopted early so no test is ever rewritten); the merge-queue states are in the
  transition table (`succeeded` is terminal only when `integrate = false`, one home:
  `model.IsTerminal`); `attempt_facts` carries the integration/cost-vector/routing/
  human-loop columns as NULL; `routines.model` is an alias into `[models]` seeded
  with `haiku|sonnet|opus`; registries are `go generate`d and migrations
  ULID-named (STYLE §11, a deliberate exception to "no code generation"); Git
  options for worktrees go through `GIT_CONFIG_*` env only; the sandbox is one
  `Sandbox.Wrap` hook on the launch path. Constitution 9 (untrusted data) added now
  — the prompt says it needs no approval. The push-policy amendment waits for
  approval (proposed wording in the addendum report). Machine facts checked:
  `bwrap` 0.11.2 and `cargo` present, `mergiraf` absent, `rerere` on,
  `conflictstyle` unset, Claude config is `~/.claude`; **`strace` is not
  installed** (M8 write-path discovery needs it or an alternative) and unprivileged
  `unshare -n` is refused — the offline proof uses `unshare -Urn` or `bwrap
  --unshare-net`.
- **2026-08-30 · M8+ fold-in review decisions.** `integrate` is frozen on Work;
  `stack_on` lives on the dependency edge; `merging` is a leased merge claim run on
  a worker (sweeper retries, `max_rebase_attempts = 3` → `conflict`); `succeeded`
  is terminal only without integration (`model.IsTerminal`) and "success" is
  `model.IsSuccess`; facts are computed at attempt-terminal with a one-time fill of
  the integration columns (the sole immutability exception); worktrees awaiting
  merge are kept by their own cleanup row and the local task branch is
  fast-forwarded after a merge so the normal removal rule applies; `fake-claude`
  is a hidden subcommand behind the one template executor (fixtures hand-authored
  first, `meta.toml` schema fixed in DESIGN §7.4); `attempts.model` is the resolved
  id, `model_alias` the alias; `[runners]`/`[models]` are embedded defaults
  overridden per key; a per-attempt MCP token is minted at claim from M1 and `forge
  mcp` runs inside the sandbox behind a worker-side attempt-scoped socket (M8);
  the claim carries a `policy` block so the worker has no second config; the
  `deps` pre-step is a merge-queue commit, never a direct push; `forge task
  requeue` resolves `conflict`. `goimports` joins `fmt-check`; `just
  check-offline` is the STYLE §11 proof. `protocol` may import `model` (wire types
  carry model enums); `model` imports nothing.
- **2026-08-30 · M1 build decisions.** `task add --wait` polls every 2 s in M1
  (SSE and `task logs -f` are M6); `daemon restart` is stop-then-start in M1 (the
  draining exec is M6); the model alias table is a fixed map in the server until
  M10's `[models]`; the worker's repository tools/MCP server arrive in M2, but the
  per-attempt MCP config and token are written from M1 so the executor command is
  final. Every test that needs an agent runs the real `forge fake-claude` binary
  built in `TestMain`, through the real executor/supervisor/parser path; the six
  fixtures are hand-authored from the recorded haiku stream shapes. Interrupted
  subagents left three real bugs that tests caught: a five-goroutine WaitGroup
  counted as four, a verify closure that captured `req` before `git_inspect` filled
  it, and a resume that never bumped `launches`.
- **2026-08-30 · The Forge repo's origin is a local bare mirror.** Repositories
  require an `origin`, and the spec forbids pushing anywhere — so `forge init`
  logic (M1: done by hand, recorded here) creates `~/.forge/mirrors/forge.git`
  with `git clone --bare` (a fetch into the mirror, never a push from the
  checkout) and adds it as `origin`. `origin/main` then exists for base
  resolution; the mirror is refreshed the same way. M9's push policy will make
  the integration branch story explicit.
- **2026-08-30 · M2 decisions.** `forge kb check` is file-based so `just check`
  passes offline; fact links are verified only when a daemon answers, otherwise
  they are warnings. Duplicate note ids are structurally impossible (id must
  equal the filename stem; stems are unique), so the check has no duplicate rule.
  The kb CLI splits file-local subcommands (new/resolve/graph/check/export) from
  index-backed ones (search/backlinks/links, served by the daemon). `forge mcp`
  implements MCP by hand (line-delimited JSON-RPC; the mark3labs dependency was
  declined — the needed surface is ~5 methods). Repository tools run inside
  `forge mcp`; fact/kb/control tools run in the daemon behind
  `POST /api/v1/tools/{name}`, both accepting the worker token or the attempt's
  MCP token. Prune compresses transcripts after 7 days (mtime preserved so
  retention still keys on age) and never touches rows. The Forge repo now
  declares its checks in `forge.toml` (fmt/vet/test — the fast subset).
- **2026-08-30 · Known gap: the Forge mirror goes stale.** The local bare mirror
  that serves as the forge repo's `origin` is only updated by an explicit
  `git --git-dir ~/.forge/mirrors/forge.git fetch ~/Projects/forge main:main`;
  attempts on the forge repo run at whatever `main` the mirror last saw (smoke 13
  first ran at a base without `forge.toml`). Candidate fixes, decided in M6:
  `forge doctor` flags a stale mirror, and the worker refreshes a `file://` origin
  mirror before fetch (a fetch into the mirror, still never a push).
- **2026-08-30 · M3 smoke findings.** The needs_input envelope is now enforced
  with `--json-schema` for ask/checkpoint attempts (`worker.EnvelopeSchema`) —
  prose questions parse as nothing and the pause was lost; the per-mode schemas
  of M4 extend this envelope. A wedged hand-started worker held the data-dir
  lock while no live worker existed: `ensureWorker` now watches a held lock and
  spawns when it frees. `task show/cancel/answer` accept unique id prefixes.
  Observed in the wild: normal-class work deferred by
  `forecast_over_target:seven_day` computed from the real account's rate spike —
  the policy behaving as designed; interactive bypasses it.
- **2026-08-30 · M4 decisions.** Greenfield ships as option (a) per the human.
  Per-repo `.forge/` directory (the human's ask, interpreted as the repo-scoped
  Forge home — flag in the report if a different meaning was intended):
  `.forge/config.toml` beats top-level `forge.toml`; `.forge/modes/<mode>.md`
  prompt overlays; `.forge/notes/` indexed into the kb. Prompt assembly now
  splits the hashable template (preamble + overlay + autonomy block + routine
  prompt) from the rendered prompt (+ context block) — `Claim.PromptTemplate`
  carries the former so PromptVersions stay comparable across attempts. The
  envelope types moved to `protocol`; `worker` aliases them. The `-builtins`
  sentinel as a mode's first allowed tool maps to `--tools ""`.
- **Third-party modules** (why): `modernc.org/sqlite` — SQLite without cgo so the
  binary builds anywhere Go does; `BurntSushi/toml` — the config format the spec
  fixes; `robfig/cron/v3` — cron parsing only, `Next()` is computed by Forge;
  `mark3labs/mcp-go` — the MCP wire protocol, evaluated in M2 (fallback: hand-written
  stdio JSON-RPC); `golang.org/x/sync` — errgroup. Added when first used, not before.

## Cuts and deferrals

- **M0:** no `bench/threshold.txt` and no `scripts/bench-check.sh` until the first
  benchmark exists (M1); `just bench`, `kb-check`, and `ui-test` print "not yet"
  until their milestones so `just check` is honest about what it covers.

## Review findings not fixed

(none yet)

## Open questions

- See the M0 report; resolved answers are moved into Decisions with the date.

- `TestServeShutsDownOnCancel` can flake under full-package `-race` load (5s graceful-shutdown deadline exceeded once, passes alone and on rerun); widen the deadline if it recurs.

- **A/B revert nuance**: the auto-revert restores generation N-1, which is usually the last human-approved snapshot but could itself be proposal-applied if two routine proposals were approved back-to-back without K runs between them; walking back to the last `edit`/human-source generation is a possible refinement.

- **daemon status shows worker pid 0** after an exec-restart or when the worker was adopted rather than spawned (the pid is only recorded at spawn); derive it from the data-dir lock holder in M7.
- **service install with a daemon-spawned worker running**: the old worker holds the data-dir lock, so forge-worker.service flaps until that worker exits (hand-over: kill the old worker, reset-failed, start the unit). init --service could offer this hand-over.
- **smoke-m6.sh step 5 journal check** reads only the first page of /api/v1/journal; page to the tail (the daemon.draining/daemon.restarted rows are there — verified by hand).

## M8 sandbox — live-verification findings (overnight, needs morning review)
- The mise-shim `claude` on PATH cannot run inside the sandbox (it tries to
  fetch/install via network and hits the netproxy deny). The LIVE ~/.forge/worker.toml
  executor command was repointed at the resolved real binary
  (~/.local/share/mise/installs/claude/<ver>/claude). Bootstrap still writes bare
  "claude"; doctor/init should resolve the real binary for the sandbox, or the mise
  shim path be added to claude_write_paths + PATH. MORNING ITEM.
- A sandboxed ssh-probe attempt's result listed HOME as containing .cache/.claude/
  .claude.json/.forge/.local/Projects but NOT ~/.ssh (keymat grep = 0, the security
  goal held). Confirm whether that home view is the sandboxed tmpfs (with bind mounts
  for .claude write-paths + the .forge socket) or leakage of the real home — the
  presence of Projects/.local is unexpected. Verify $HOME is a clean tmpfs and no
  sibling checkout under ~/Projects is visible. MORNING ITEM (security).
- **2026-08-30 · M9 build decisions.** The integrator is DAEMON-side V0
  (`internal/integrator`, its own package so it may import both `worker` — Git,
  RunChecks, ForgeToml — and `store`, which controlplane-by-convention and worker-by-rule
  cannot combine): one serial loop per daemon, oldest `queued_for_merge` per repository
  per tick, git work in a scratch clone under `<home>/integrator/<repo>/<target8>`
  cloned from the REGISTERED CHECKOUT (reads only; constitution 1), integration branch
  fetched from and pushed to the checkout's real origin URL. `merging` is held without
  a lease; a crash leaves a leaseless `merging` row the next tick requeues
  (`recoverMerging`) — the DESIGN §4.1 worker-side merge claim is deferred.
  **Mergiraf wiring that actually works:** a plain `git rebase` with
  `merge.conflictstyle=zdiff3` via `GIT_CONFIG_*` env, then `mergiraf solve
  --keep-backup=false` per conflicted file, `git add`, `rebase --continue` (looped) —
  no `.gitattributes`, no merge-driver config, verified live on an adjacent-addition
  Go conflict. Anything mergiraf cannot solve → `conflict`, human queue, scratch clone
  retained (`forge task requeue` re-enters). Checks failing on the rebased result →
  `unverified` `check_failed:<name>`. Push goes through ONE function
  (`integrator.PushIntegration`): branch must be `integration_branch` or match
  `task_branches` from the repo's forge.toml, argv is `push <url>
  HEAD:refs/heads/<branch>` by construction, and a test greps the package source for
  force flags and +refspecs.
- **2026-08-30 · M9 lease rules.** Write-set leases live in `path_leases`, inserted at
  claim with the EFFECTIVE globs (`controlplane.EffectiveGlobs`: undeclared = `["**"]`;
  deps or lockfile-touching globs widen with the implicit exclusive lockfile lease
  go.mod/go.sum/package.json/*.lock) and deleted by `Transition` when the Target
  leaves `model.HoldsWriteSet` (claimed/preparing/running/verifying). Intersection is
  conservative-with-proofs (`globPairIntersects`): literals compare with a `/`
  boundary, globs by literal-prefix divergence, and single-segment globs (`*.lock`)
  meet slash-carrying globs only at the bare first directory — so `docs/**` vs
  `internal/*` and `*.lock` vs `docs/**` are disjoint while anything vs `**`
  intersects. Non-writing modes (WritesNone/KbOnly: verify, plan) are LEASE-EXEMPT in
  both directions — without this the L2 verify follow-up deadlocks against its own
  subject, which holds the lease through `verifying`. `lease_wait_us` is derived from
  a one-time `target.lease_blocked` journal row written the first time a claim passes
  a Target over for `path_lease` (wall clock, blocked→claimed).
- **2026-08-30 · M9 plan/integrate modes and facts.** `plan` (L0, WritesNone,
  interactive) returns a required `tasks[]`; the daemon creates the batch inside the
  completion transaction (`planFollowUps`): one `run`-mode Work per task (deliberate:
  L1 is the level the merge queue re-checks, and an `implement` batch would multiply
  into L2 verify chains), `plan_batch_id` set, edges from `blocked_by` indexes with
  `stack_on` on the edge; the plan Work's own `integrate` flag is CLEARED at creation
  (a non-writing mode has nothing to merge — otherwise `succeeded` is never terminal)
  and travels in the snapshot as the batch-inheritance hint. Facts for integrating
  attempts are inserted at `succeeded` (before `queued_for_merge`), and the integrator
  fills merge_wait_us/rebase_attempts/merge_outcome/stack_depth NULL→value once
  (`UpdateIntegrationFacts`, refusing a second fill) — only for `merged` and
  `checks_failed`; a `conflict` is not terminal and keeps the columns NULL for the
  eventual real outcome. `touched_paths` come from the L0-checked envelope
  `changes[]`; `write_set_precision` uses the scheduler's own matcher; both NULL when
  underivable. Stacking is implemented end to end: a `stack_on` edge lets the claim
  pin `Claim.StackBase` to the unmerged dependency's branch head (recorded as
  `attempts.stack_base_commit`, surviving claim replays), the worker resolves the base
  there, and Pick refuses chains deeper than `[integration] max_stack_depth`
  (`stack_depth` facts V0: 1 when stacked, else 0).

## M9 known gaps (reported)

- **The `deps = [...]` serialized pre-step is NOT implemented.** A submission whose
  Work declares deps with `integrate = true` is refused with a 400 naming the gap
  ("M9 known gap: the serialized dependency pre-step is missing"); non-integrating
  deps-declaring Work still runs and holds the implicit lockfile lease.
- **The automatic `integrate`-mode conflict attempt is NOT spawned.** The mode ships
  (registry, preamble, schema, L1, no-web toolset) and a human can run it by hand,
  but a rebase conflict lands directly in `conflict` + human queue with the scratch
  clone retained; wiring an attempt into the integrator's scratch state needs the
  worker-side merge claim and is deferred with it.
- **Worktrees of merged Targets are not fast-forwarded** (DESIGN §4.3's "worker
  touches the manifest at merge time"): after a multi-task merge rewrites SHAs, the
  attempt worktree's head is unreachable from the remote and reconcile retains it
  with "unpushed commits" instead of removing it. Single-task fast-forward merges
  clean up normally.
- **`stack_depth` facts are 0/1**, not the true chain length; the honest chain is on
  the edges and Pick computes it, but the integrator records only "stacked or not".

## M10 runners, models, routing (DESIGN §21)

- **Sonnet is classed `mid`.** With classes frontier|mid|small|local and opus=frontier,
  haiku=small, sonnet sits between as the workhorse mid-tier model; opus stays reserved
  for the hardest (frontier) work. All three embedded models ship `max_tier = 3` so any
  routine tier can reach any of them. Prices are Anthropic list prices ($/MTok):
  haiku 1/5, sonnet 3/15, opus 15/75, with cache_read = 0.1× input and cache_write =
  1.25× input. Model ids match the M1 `defaultResolveModel` table
  (claude-haiku-4-5-20251001, claude-sonnet-4-5, claude-opus-4-1); refresh both together.
- **Embedded runner/model/routing defaults are NOT written by bootstrap** so a Forge
  upgrade refreshes prices and policy. `LoadConfig` merges the embedded defaults under
  config.toml using the decoder's `MetaData.IsDefined` — a field the operator set, even
  to a zero (an `explore = 0`, a free `price.input = 0`), is respected; an unmentioned
  field inherits the default. `WriteDefaultConfig` trims the trailing zero `[routing]`
  table the encoder emits (runner/model maps are nil and already omitted).
- **No new migration.** The initial migration already provisions every M10 column
  (attempts.runner/escalated_from/routing; attempt_facts.usd/five_hour_delta/
  seven_day_delta/runner_seconds/runner/model_class/escalated_from), so M10 only wires
  them through `AttemptFacts`, `ClaimParams`, and the scanners. STYLE §10's "never edit
  the initial migration" is honoured — nothing was edited.
- **Routing decision storage:** the JSON decision (candidates, scores, chosen, why) is
  stored on `attempts.routing` (the pre-provisioned JSON column), read by the task-detail
  UI via the `routing` template func. `escalated_from` and `runner` are their own columns.
- **Routing engages only when a routine opts in** — a `models` allowlist or an explicit
  `tier`. A plain routine (just `model`) keeps the M1 single-model path, so existing
  behaviour and every prior test are unchanged. When there is no allowlist but a tier is
  set, candidates are all models with `max_tier ≥ tier`.
- **Escalation is a deterministic ladder climb**, separate from first-attempt scoring:
  attempt N+1 of an allowlist routine uses rung `min(priorFinishedAttempts, len-1)` with
  the previous attempt's failure injected into the *rendered* prompt only (not the
  hashable template — the `escalated_from` column tracks it). First-attempt selection is
  by lowest weighted cost-vector score subject to the Wilson gate; the ladder order is
  the escalation order and the score tie-break.
- **Runner capacity is enforced at claim time** in `routeClaim` (via an in-transaction
  in-flight-by-runner count), not in `Pick`: since routing chooses the model — and thus
  the runner — at claim time, `Pick` cannot know the runner. A claim that finds no
  runnable candidate returns nil (the target stays pending and retries), which is how two
  capacity-1 targets serialise on a runner while worker slots are free.
- **Cost-per-verified-success in the two subscription windows** is summed from
  `five_hour_delta`/`seven_day_delta` facts; these are NULL for api/local runners (no
  rate-limit samples bracket them), so those columns read `-` honestly rather than 0.
- **Model-class prompt overlay** resolves the live `<home>/modes/<mode>.<class>.md` only
  (no embedded per-class default exists in the mode registry); it is composed into the
  hashable template so a class overlay changes the PromptVersion.
- **Deviation — capability matrix axis:** the spec says "tier × model", but attempt_facts
  carry the model's *class*, not the routine tier, so the matrix rows are per-model with
  the class shown as the capability axis (tier is a routine property, not an attempt one).
- No new third-party dependencies.
