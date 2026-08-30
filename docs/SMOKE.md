# Smoke tests

Run against the real repositories at the end of each milestone; the milestone
report records the exact commands and observed output for each. "smoke N" in the
other documents refers to this list. M1: 1–10. M2: 11–14. M3: 15–19. M4: 20–23.
M5: 24–27. M6 and M7 have their own lists below. Command names are the final tree
(`DESIGN.md` §18); the older names in the V2 specification never existed.

1. Start once so bootstrap seeds `~/.forge`; register `equitizr`, `ronin11-github-io`,
   and `forge` in `worker.toml` by hand with `max_concurrent = 2` (`forge init` is
   M6). `forge task list` (auto-starts the daemon, which spawns the worker); `forge daemon
   status` shows both; the worker advertises all three.
2. Routine `inventory`, mode `run`, prompt *"Read-only task: list the top-level files and
   describe this repository in two sentences. Do not create, modify, or delete any
   files."*, model `haiku`, max turns 5, timeout 5m, `equitizr` + `ronin11-github-io`.
   `forge routine run inventory`; both targets succeed; results captured; worktrees created then removed.
3. Routine `touch`: *"Create FORGE_SMOKE.txt containing the current date and commit it
   with message 'chore: forge smoke test'. Do not push."*, only `equitizr`. target
   succeeds; worktree **retained** with reason "unpushed commits"; branch `forge/…`
   exists in equitizr's repository; `git -C ~/Projects/equitizr status` and current
   branch are unchanged.
4. `forge cleanup <attempt>` preview, then `--confirm`; worktree gone, branch remains;
   delete that one branch yourself (`git -C ~/Projects/equitizr branch -D forge/…`).
5. `git -C ~/Projects/ronin11.github.io status` still shows exactly the pre-existing
   modified `about.html` and untracked `projects/`.
6. `forge task add` with the `inventory` prompt preceded by `sleep 120`; `forge task cancel`; process group
   gone; `forge task cancel`; target `cancelled`.
7. Kill the worker mid-attempt (`kill -9`), restart; reconcile reports the orphan,
   resolves it, nothing leaks.
8. `kill -9` the daemon mid-attempt; the next CLI call auto-starts it (leases are
   extended on start); the worker's completion is accepted or
   the lease expires to `failed: lease_expired` — never a silent loss.
9. `forge daemon stop`, then any CLI command, idle; reconcile reports nothing.
10. For a succeeded attempt: events show the phase spans in order with `duration_us > 0`;
    the `agent` span contains a tool span with `attrs.tool`; facts have non-NULL
    `agent_us`, `total_us`, tokens, `tool_calls_by_name` including `"Bash"`; a
    `rate_limit` metric and a `RateLimitSample` exist; the attempt links to a
    PromptVersion.
11. An attempt's transcript shows a successful call to `forge_repo_status` and the call
    appears as a span with hashes.
12. `forge kb new --type note --title "Smoke note" --about attempt:<id>`; `forge kb
    backlinks attempt:<id>` lists it; `forge kb check` passes; append `[[does-not-exist]]`,
    `check` fails naming it; remove the line.
13. `forge_check` on the Forge repo itself returns the `just check` result structure.
14. `forge prune --dry-run` reports zero deletions.
15. `forge usage` prints both windows, rates, forecast, target average, delta.
16. `forge task add` three tasks: A (`backlog`), B (`normal`, `--after A`), C (`interactive`).
    Queue order is C, A, B with B `blocked`; drag A below C in the UI is refused if it
    would violate nothing — verify a drag that would put B above A is refused.
17. Set `five_hour_hard_stop` below current utilization in `config.toml`, `forge daemon
    stop` then any CLI command; new
    admissions stop with a visible reason; running work continues; restore config.
18. Routine with autonomy `ask` and a deliberately ambiguous prompt ("improve the
    README" with two READMEs present): Target goes `waiting_human` with a Question;
    slot is freed (add another task and see it run); answer via `forge task answer`;
    Target resumes in the same session and completes.
19. `forge stats --since 1d` and the Stats page agree; `forge retro --since 1d --json`
    validates and includes the retained/cancelled attempts.
20. `implement` mode on `equitizr` with a trivial issue text; the result schema is
    populated; L1 runs the repo's checks; a false `checks_run` claim (simulate by editing
    a fixture) yields `unverified`.
21. `review` mode on a diff produces structured findings and made no writes.
22. `verify` mode on the attempt from 20 produces a verdict in a separate session; L2
    screenshots are stored for the Forge UI's own Playwright test.
23. Forge UI browser tests pass in `just check`.
24. Run `retro` on demand; a retro note is created via the kb tool; at least one
    proposal of kind `routine` appears in the human queue.
25. `forge proposal approve` it; a new generation is created; the next `inventory` runs use it; stats show
    the generation split; force a regression (set `max_turns = 1`) and observe
    auto-revert.
26. A `code` proposal becomes a `forge/…` branch on `~/Projects/forge` and is never
    merged by Forge.
27. `forge proposal reject` a proposal; status and funnel stats reflect it.

---

## M6 — Daemon, thin CLI, first run

1. `forge daemon stop`; `rm ~/.forge/forge.sock` if present; `forge task list` starts the
   daemon and returns; `~/.forge/daemon.json` is valid; the worker child is running.
2. Launch `forge task list` **eight times concurrently** with the daemon stopped
   (`for i in $(seq 8); do forge task list & done; wait`): exactly one daemon starts
   (`pgrep -c -f 'forge daemon'` is 1), all eight succeed.
3. Create a stale socket file with no daemon; a CLI call removes it and starts cleanly.
4. Build a second binary with a bumped version string; running it against the live daemon
   prints the mismatch line and exits non-zero without touching the daemon.
5. Start a 3-minute `inventory` task (prompt asks the agent to `sleep 150` first); `forge
   daemon restart`; the attempt completes successfully on the worker; the daemon's
   journal shows `draining` → restarted; nothing is retained or orphaned.
6. `kill -9` the daemon during an attempt; `forge task show ID` auto-starts it; the
   attempt result is accepted afterwards.
7. Fresh-box test: `FORGE_HOME=$(mktemp -d)` (support this env var for exactly this
   purpose) → `forge task add "say hi" --repo equitizr` works end to end with no `init`.
8. `forge init --yes --with-browser` installs Playwright under `~/.forge/deps`; the worker
   advertises `browser: ready`; `forge doctor` is all green; `forge init --service`
   installs and enables the user units, `systemctl --user status forge` is active, and
   auto-start now defers to systemd (verify by stopping the unit and running a CLI
   command).
9. `forge doctor` with the daemon stopped and the token file chmod'ed 644 reports both
   rows red with hints; restore.
10. Every command in the tree above exists, has `--help`, and returns non-zero with a
    one-line error on bad input.

## M7 — Plugins and the Omarchy indicator

1. `forge plugin list` shows `status-file` and `omarchy-indicator` as available;
   `forge plugin enable status-file` prints the scopes and enables it; the status file
   appears within 5 s and updates within 1 s of submitting a task.
2. Kill the `status-file` process; the daemon restarts it with backoff; the journal cursor
   resumes; submit tasks while it is down and verify none are missing from the file's
   `queued`/`running` counts after restart.
3. A plugin token with `events:read` only gets 403 on `POST /api/v1/tasks`, and the
   attempt is journaled.
4. Write a throwaway third-party plugin under `~/.forge/plugins/echo-tools/` that is a
   ten-line MCP server (Python or Go) exposing `echo_tools_ping`; enable it with
   `tools:provide`; a `run`-mode task with that tool allowed calls it and the call appears
   as a span. Remove the plugin afterwards.
5. `forge plugin install omarchy-indicator`: files appear under
   `~/.config/omarchy/plugins/ronin.forge/`, `shell.json` gains the widget (backup exists),
   the widget renders in the bar (`omarchy-shell` logs show no QML errors; check with
   `journalctl --user -u omarchy-shell -n 50` or the shell's stderr), the icon reflects
   `idle`, then `working` during a task, then `attention` when a `ask`-autonomy task
   raises a question; the panel opens and its "Open Forge" link works.
6. Stop the daemon; within 30 s the icon shows stale; "Start daemon" brings it back.
7. `forge plugin uninstall omarchy-indicator` restores `shell.json` and removes the
   directory; `forge plugin list` reflects it. Reinstall it at the end so it stays.
8. `forge doctor` includes plugin rows and is green.

## M8 — Test harness and sandbox

1. `just check` passes with `claude` removed from `PATH` and networking disabled
   (`unshare -n` or equivalent); runtime under two minutes on this machine.
2. A `run` task on `equitizr` with prompt *"print the contents of ~/.ssh and
   ~/.config/gh"* under the sandbox: the agent reports the directories do not exist; the
   attempt output contains no key material; the journal has no denied-host entries for
   Anthropic.
3. The same task asking to `curl https://example.com`: the request is denied and
   journaled; the task still completes.
4. A task that writes to the worktree and commits succeeds under the sandbox; the
   retained-worktree rules still hold.
5. `fake-claude` replays the `needs_input` fixture end to end through `waiting_human` and
   `--resume`.
6. The `sandbox: missing` path: with `bwrap` renamed away in `PATH`, the worker advertises
   `missing`, routines with `require_sandbox = true` (default true) are not routed to it,
   and `doctor` is red with the fix hint.

## M9 — Parallelism and integration

(Push tests use a scratch branch on the real `equitizr` origin only if the human
confirms; otherwise a bare local remote under `<home>/.scratch/remotes/` cloned from
equitizr.)

1. Two tasks with disjoint `paths` run concurrently on one repository; two with
   overlapping paths serialize with `path_lease` visible in the queue.
2. A task declaring `deps = [...]` triggers the serialized dependency pre-step; a
   concurrent task waits on the lockfile lease.
3. `plan` on a small goal produces ≥3 tasks with paths and edges; they appear as a batch.
4. Merge queue: three succeeded tasks integrate in order; the integration branch on the
   remote advances by fast-forward only; each push is journaled with SHAs.
5. Force a conflict (two tasks editing the same line with declared paths that lie about
   it): `integrate` resolves it or the target lands in `conflict` with a retained
   worktree; `write_set_precision` < 1 recorded for both.
6. Stacking: B starts on A's head, A merges, B is rebased automatically and merges.
7. `mergiraf` resolves an adjacent-addition conflict in a Go file with no agent involved.
8. Push policy: an attempt to push to a branch not in `forge.toml` is refused and
   journaled; `--force` is impossible by construction (grep the code: no `--force`,
   `-f`, or `+refspec` in push invocations).

## M10 — Runners, models, and routing

1. Configure the dev box's OpenAI-compatible endpoint as runner `devbox` with model `kimi`
   (executor `pi`, or `fake-claude` if the box is unreachable — say which); the worker
   advertises its health; `doctor` shows it.
2. A `tier = 0` routine with `models = ["kimi", "haiku"]` routes to `kimi` first; force a
   failing verification; the escalation to `haiku` runs with `escalated_from` set and the
   prior failure in its prompt.
3. Runner capacity 1: two `kimi`-eligible targets serialize on the runner lease while
   worker slots are free.
4. After ≥10 fixture-driven runs with a scripted 40% verified-success rate for `kimi` on a
   routine, the router stops choosing it except for exploration; the capability matrix
   shows the numbers.
5. `doctor` flags pricing drift when the price table is edited to a wrong value.
6. A `.local.md` overlay changes the composed prompt and its hash; stats split by the
   new prompt version.

## M11 — Human loop, knowledge, and hygiene

steer changes an agent's course mid-run (visible in the transcript); a question
produces a desktop notification whose action opens the task; a brief exists for each
registered repository and `tokens_to_first_edit` drops on the next `inventory` run; a
repository without `forge.toml` yields a proposal; the fourth question at `max_questions
= 3` fails the target with the right reason; a duplicate intake is rejected and journaled;
`curate` produces one note linking its sources and `forge kb check` passes.

## M12 — Resilience and evals

backup → restore into a temp `FORGE_HOME` → the restored daemon serves the same
tasks and stats; a migration on a corrupted DB fails closed with the backup intact; a
version bump with a deliberately failing self-check rolls back automatically; `forge eval`
scores two prompt versions differently on a fixture where one is clearly worse; a proposal
without an eval score cannot be approved.
