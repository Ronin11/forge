# Smoke tests

Run against the real repositories at the end of each milestone; the milestone
report records the exact commands and observed output for each. "smoke N" in the
other documents refers to this list. M1: 1–10. M2: 11–14. M3: 15–19. M4: 20–23.
M5: 24–27.

1. `forge init`; register `equitizr`, `ronin11-github-io`, and `forge` in `worker.toml`
   with `max_concurrent = 2`. `forge run` in the background; the worker advertises all
   three.
2. Routine `inventory`, mode `run`, prompt *"Read-only task: list the top-level files and
   describe this repository in two sentences. Do not create, modify, or delete any
   files."*, model `haiku`, max turns 5, timeout 5m, `equitizr` + `ronin11-github-io`.
   Submit; both Targets succeed; results captured; worktrees created then removed.
3. Routine `touch`: *"Create FORGE_SMOKE.txt containing the current date and commit it
   with message 'chore: forge smoke test'. Do not push."*, only `equitizr`. Target
   succeeds; worktree **retained** with reason "unpushed commits"; branch `forge/…`
   exists in equitizr's repository; `git -C ~/Projects/equitizr status` and current
   branch are unchanged.
4. `forge cleanup <attempt>` preview, then `--confirm`; worktree gone, branch remains;
   delete that one branch yourself (`git -C ~/Projects/equitizr branch -D forge/…`).
5. `git -C ~/Projects/ronin11.github.io status` still shows exactly the pre-existing
   modified `about.html` and untracked `projects/`.
6. Submit `inventory` with a prompt that runs `sleep 120` first; cancel; process group
   gone; Target `cancelled`.
7. Kill the worker mid-attempt (`kill -9`), restart; reconcile reports the orphan,
   resolves it, nothing leaks.
8. Kill the control plane mid-attempt; restart; the worker's completion is accepted or
   the lease expires to `failed: lease_expired` — never a silent loss.
9. Stop and restart `forge run` idle; reconcile reports nothing.
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
16. Submit three Works: A (`backlog`), B (`normal`, `--after A`), C (`interactive`).
    Queue order is C, A, B with B `blocked`; drag A below C in the UI is refused if it
    would violate nothing — verify a drag that would put B above A is refused.
17. Set `five_hour_hard_stop` below current utilization in config, restart; new
    admissions stop with a visible reason; running work continues; restore config.
18. Routine with autonomy `ask` and a deliberately ambiguous prompt ("improve the
    README" with two READMEs present): Target goes `waiting_human` with a Question;
    slot is freed (submit another Work and see it run); answer via `forge answer`;
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
25. Approve it; a new generation is created; the next `inventory` runs use it; stats show
    the generation split; force a regression (set `max_turns = 1`) and observe
    auto-revert.
26. A `code` proposal becomes a `forge/…` branch on `~/Projects/forge` and is never
    merged by Forge.
27. Reject a proposal; status and funnel stats reflect it.

---
