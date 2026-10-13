# Six one-person factories, read against Forge

Research note, 2026-10-11. Source: the r/ClaudeAI thread "Share your
completely autonomous software assembly line" (post 1x0ztjs, 9 comments)
and the six repositories it links. Each repository was read at mechanism
level (workflow YAML, prompts, config, commit and PR history) on 2026-10-11.
Forge's side of every comparison was checked against the code at 4559dae,
not from memory. Anything not read is marked.

## 1. What the thread actually contains

The post asks for public repositories run by one person at 10k+ lines and
dozens of PRs a day. Nobody in the thread claims one. The useful replies:

- The OP's own research listed five repositories (Jovie, 0509, evener,
  agent-afk, continuum) as examples of 200+ commits a day. Measured over
  their last 100 commits, none reaches that. Jovie squash-merges about
  75 PRs a day with no human before merge. 0509 merges 15 to 25 a day.
  evener and continuum merge 50 to 60 a day but every one is a human
  clicking merge on a Claude Code session's PR. agent-afk is about 40 a
  day, owner-merged.
- orgtree (u/DynaBeast): a Windows multi-agent org chart, not a factory.
  No verification gate, no worktree sandboxing, and the 4.0 rewrite
  removed the landing slots and evidence receipts the 3.x version had.
  Asked how the code stays under control, the author says the agents
  work in git worktrees and "coordinate to land code in turns" before
  merging, pushing, building and publishing on their own, with a design
  walkthrough before and an implementation walkthrough after each major
  feature that the author reviews, then tests by use. The repository at
  its tip does not enforce any of that; it is convention carried by the
  agents' prompts and by the author. Two ideas worth keeping, in
  section 3.
- u/Ok_Gur_9033 runs a deliberately non-autonomous pipeline on a run file
  (each step writes status, an interrupted run says how far it got,
  blocked items retry first next run) and asks the right question: "what
  tells you a night's output was wrong before you build on top of it?"
- u/darrylwolfaardt: the line is rarely the bottleneck, the input is.
  Write the business rules, terms and states down and have the person who
  knows the business confirm them before the agents run.

Two of the six are factory-on-factory loops. Jovie's last 24 hours of
commits are almost entirely fixes to its own lanes. fleet-ops recorded a
tripwire on 2026-09-22: 149 merged PRs on the orchestrator against 45 on
the product, 100 open issues all self-filed by the worker bot, and
enrolled itself out of its own intake. Forge is in the same regime and
should measure the same ratio.

## 2. Where the others are ahead of Forge

Ordered by how much each would have changed the last month at Forge. The
2026-10-10 reliability audit (supervisor dead eleven days, Signal inbound
never worked, jobs failing mute) is the reference point.

### 2.1 Alerts become work, and the outside dead-man

Jovie's doctor runs at the end of every tick, not daily. Every alert key
(`tick-error`, `provider-down:<lane>`, `provider-idle:<lane>`,
`codex-all-banked`, `linear-down`, `pool-empty`, `no-landing`,
`gate-timeouts`, `disk-low`, `github-quota`, `hud-stale`, `orphan-prs`,
`aged-prs`, `spawn-exit`) opens a triage issue in the same queue the
lanes pull from, and re-opens it within six hours if it closes while the
condition stands. fleet-ops does the same from Grafana: alert rules post
to the GitHub issues API with the `agent-ready` label, so a dead router
row or a spend spike is a queued task before anyone reads it. A CI test
walks the notification policy tree and fails if the set of rules that
page the human differs from a declared list.

Every fleet-ops oneshot and timer unit carries
`OnFailure=fleet-unit-failed@%N.service`, which pings a healthchecks.io
fail URL. Prometheus runs a `vector(1)` Watchdog rule routed every five
minutes to an outside dead-man URL. Units with `Restart=always` get
`ExecStopPost` instead, because `OnFailure` never fires for them. A
`paid-plan-idle` rule fires when a provider has zero successful responses
in 24 hours and labels it quota-wall or idle from the 429 count.

Forge today: `forge doctor` daily at 07:00, alerts announce once then
join a daily digest, nothing opens a task, no dead-man outside the box.
The audit's pass-two detectors are not built. Every one of the audit's
silent failures maps to an alert key above (`no-landing`,
`provider-idle`, `spawn-exit`, `hud-stale`).

### 2.2 Jev at the front door and at lane choice

fleet-ops uses the same TypeSafe Jev that Forge does, but at four places
Forge does not. The question text is the mechanism:

- Admission, on every owner-filed `needs-triage` issue: "Admit this issue
  for an automated worker now? Yes only if the described change is
  concrete, scoped to this repository, with a clear way to tell it is
  done, that a coding worker can finish without asking anyone anything
  and without a product, pricing, legal, security or customer-data
  decision." p >= 0.9 admits, else it parks for the orchestrator. A
  regex exclude (secrets, money, customer-data deletion) skips Jev
  entirely. Daily cap of 10 admits, counted from the comments so a
  re-run cannot over-admit.
- Lane choice: one choice question, `judgment` versus `mechanical`.
  p >= 0.9 goes to Claude, p <= 0.1 to the free lanes, anything between
  or an error goes free. Held-out eval of 60 issues labelled by a
  different model family: 24 of 24 in the Claude band correct, zero
  mechanical issues sent to Claude.
- Review verdict: the reviewer model writes prose, Jev decides
  `approve`/`block` on "the review lists no blocker line; risk and nit
  lines alone do not block", p >= 0.9. "Claude never grades Claude."
- Miss review class: `packet-unclear | rule-gap | infra | one-off`.

Forge today: Jev runs only on job directive steps that declare outcomes.
The intake interview and concierge are the admission gate and they are
prose, not a verdict. The execution-flow doctrine already says
"operation unless judgment", which is exactly the lane-choice question.
The economist's experiment arms could take the band as a feature.

### 2.3 Prompt-cache hit as a paged metric

fleet-ops alerts when `cached / input < 0.5` for 30 minutes on any route
with more than 1000 input tokens a second, with a promtool fixture
proving the rule can fire. Its router config requires every model row to
carry a measured `cache_proof` string, and a CI step renders the worker
prompt for two different issues and byte-compares everything before the
`=== FLEET-DYNAMIC-BELOW ===` marker. A worker stays on one credential
seat from gate to last call "which keeps the prompt cache".

orgtree fingerprints the prompt prefix component by component (system
prompt, tool list, model, effort, permission mode, folders, account),
reads the provider's cache receipt and TTL, and publishes a green/red
cache forecast per agent. Its auto cheap-compaction fires only before a
turn whose cache is known cold and whose context is above a threshold.

Forge today: the prefix order is pinned by a unit test and cache tokens
are recorded and priced, which is more than most. Nothing watches the
ratio, and `continuation=fresh` triggers on size alone, not on a cold
forecast. The 2026-09-22 token findings make this the cheapest cost
lever on the list.

### 2.4 Infra failure is not a strike

fleet-ops classifies every failed run before it counts. Infra when the
worker step started and never completed, or was cancelled inside a
failed job (runner lost), or failed in under 600 seconds with a router
error in stderr, or hit the 55-minute wall while host load was above
5 x cores or swap above 4 GiB (sampled seconds after death). Infra
re-queues without a strike, capped at three since the last park. The
real payloads from the 2026-10-05 overload are checked in as test cases
and replayed in CI. A first strike re-runs on the strong lane with the
issue thread and the pushed branch as context. A second parks for an
Opus orchestrator with 80 turns and a daily cap of 10, and if that
produces no PR, only a person clears the label.

Forge today: the mechanic classifies load-flake, landing-conflict,
ratchet, turn-cap and clean-tree, once per lineage. There is no host-load
sample at death, no "stronger model with the failed branch as context"
rung, and the audit found the supervisor rung was not running at all.

### 2.5 Risky paths hold for a non-same-family verdict

fleet-ops `risky-paths.json` lists path regexes (secrets, billing,
pricing, payments, deploy workflows, wrangler config, and for the
orchestrator repo every prompt, config and workflow file) and diff-line
patterns (`data-delete`, `row-rewrite`, `object-purge`,
`workflow-secrets`, `prod-deploy`). A `hold-risky` job that never checks
out code disarms auto-merge, dequeues, and labels. Approval is a human
review, or a review by Opus/Fable (Fable only when Claude built the
change) whose prose Jev judges, plus a second GLM-family reviewer for
guard paths. Every failed read holds. 70 classify and approval cases
live beside the config and CI runs the real jq against them, including
a forged `coordinator-approval:` line in the diff.

Forge today: protected paths and `forge.toml` are refused unless
`--allow-protected`. Refusal is safe but it means every change to
Forge's own prompts and config is either hand-landed or blocked. A hold
with an independent verdict is the middle setting Forge lacks.

### 2.6 Merge is not done

Jovie's Linear sync moves an issue `Merging` to `Validating` to `Done`
only when every receipt in a `validation-manifest:v1` block passes, and
to `Rework` otherwise. Receipt kinds: `outcome`, `human-certification`,
`screen-audit`, `founder-taste`, recorded against a verified production
SHA. continuum's rule, written by its own agents on 2026-10-11: "When a
change can only be measured after deploy, the Measured section names the
check, its owner and its deadline, and the card stays open until the
receipt is posted."

Forge today: on-landing deploy runs smoke and the deploy look, records a
`deploys` row and rolls back, but "a deploy failure never touches the
task's own state". Closure is not gated on the receipt.

### 2.7 Lessons bound to guards

Jovie's `LESSONS.guards.json` links every lesson heading to a named
regression test or scoped rule, with a reason and a SHA-256 of the
reviewed lesson, validated by a script. `canon/invariants.jsonl` gives
each invariant an id (`JOV-INV-020`) that CI steps and PR bodies cite.
0509's eslint config carries the issue or commit each rule came from in
the rule's own message. fleet-ops: "A new schema rule gets a reject file
in the same PR", 36 reject files each breaking one rule, and
"a rule that recurs twice is a defect in its rung".

Forge today: hidden tests fold into the standing suite on landing and
allowlist tests cite task numbers. There is no ledger that says which
guard holds which lesson, so a guard can be deleted without anyone
noticing the lesson is unprotected.

### 2.8 Miss review: a post-mortem on every failure, with a trigger

fleet-ops starts a miss review on `agent-failed`, reopen, a `park:`
comment, a claim PR closed unmerged, or a merged revert. It is never on
a clock. The reviewer posts `miss-review: <class> | <root cause> |
evidence:`. The rule that makes it a loop: "When 2 or more miss-review
comments in this same repo name the same rule-gap root cause, open one
PR that changes one prompt line or one config value."

Forge today: the learning stack supervises continuations and files
follow-ups, and `src/audit.rs` maps tool logs to cost anti-patterns.
Nothing classifies each failure into a small fixed set, and nothing has
the "same root cause twice means a one-line fix" trigger.

### 2.9 Smaller items

- **Wind-down before the wall** (agent-afk): a soft deadline at about
  85 percent of the budget strips tools and lets the child write its
  handoff. "No observable output for 8 minutes" aborts. Forge has the
  wall and the no-edit watch but no wind-down, so a capped attempt loses
  its uncommitted state.
- **Open-work budget** (Jovie): a lane holding `slots x 2` open
  non-green PRs only fixes or adopts until it drains. Declared paths are
  compared with the open-PR inventory for overlap, and a hotspot path
  with an open PR holds new work. Forge's 38 percent of spend on
  edited-but-not-landed work is the same symptom.
- **Adaptive slots** (Jovie): scale up every 30 minutes only while
  merged per hour is at or above opened per hour and queue wait p50 is
  under 30 minutes. Forge has fixed slots and `forge stats` has the
  inputs.
- **Worktree pool** (Jovie): pre-installed pool slots refilled in the
  background, sweep retires checkouts an hour after PR close, dirty work
  pushed to a backup branch. Forge clones fresh and the audit found gc
  kept 474 of 478 worktrees.
- **Stop revokes publication** (Jovie): a kill writes a revocation
  record and every irreversible boundary (push, PR, label, enqueue)
  re-reads it. Forge checks abort before each step, not at push or land.
- **Revocation of same-group retries** (fleet-ops): `num_retries: 0`
  with fallbacks on first failure, because "a same-group retry re-hits
  the same wall" and a 2/2/2 retry policy "re-sent the cold prompt to a
  third seat". Forge's Jev retry is three with backoff.
- **Mutation-proof ratchets** (continuum): the pipefail ratchet strips
  pipefail from a real script on every run and asserts the gate goes
  red. "The ratchet must be able to FAIL, or it is decoration." Rust and
  ESLint ratchets also fail on shrink without lowering the baseline.
- **Promotion by tree equality** (continuum): canary promotes to main
  with `-s ours` and an assertion that `HEAD^{tree} == canary^{tree}`,
  and the deploy verdict counts only check-suites started by the tip's
  own push event. The motivating incident: main was six weeks behind
  canary until someone noticed.
- **Morning answer to the thread's question**: continuum's deploy
  tracker posts a post-deploy self-check into the project room and
  writes a HOLD file on failure that only a human clears. Forge's assess
  rule never blocks and a night's concerns go to a digest.

## 3. What orgtree has that nobody else does

- Event watchdogs with a silence mode: fire when no matching event
  arrives for a quiet period, with threshold crossings only, per-
  subscription bounded queues, and a documented ping-pong limit. Forge's
  job triggers are cron, message, webhook and events at an offset, and
  the audit wants plugin-silence detection. Silence mode is that.
- Usage-limit handling that freezes an agent until the stated reset plus
  60 seconds, marks the account limited, and auto-continues with a short
  prompt. Forge has the equivalent for the claude window; orgtree adds
  account fallback that keeps the replacement account and notes that a
  switch starts a new cache.

## 4. What Forge has that none of them do

Worth stating so the gap list is not read as a deficit list.

- L1 re-runs the repository's own checks from the trusted base with
  hidden tests overlaid, and a claimed pass Forge cannot reproduce is a
  failure. Jovie trusts `pre-push-gate.sh affected` in the agent's tree.
  0509's proof is `preview-assert`, which is close. evener and
  agent-afk re-run gates in the orchestrator but from the agent's tree.
- A serial integrator with a pre-receive guard, rebase and full re-check
  on the merged tree. Everyone else uses GitHub's native merge queue,
  which 02-merge-queue already found reduces to the same thing.
- Experiment arms with provider re-draw on hold, USD caps at three
  levels checked against a projected next-attempt cost, and the
  `run_json` cursor that resumes a crashed run at its step.
- Red-on-base for the tests step. Only evener's fu-3 report does this,
  by hand, per round.

## 5. Order of work, if this note is acted on

1. Alert keys that open tasks, a per-tick doctor, and an outside
   dead-man on the worker and supervisor units (2.1). This is the
   audit's pass two with a known shape.
2. Jev admission and the judgment band at claim (2.2). Forge has every
   part already; it is two questions and a cap. **Built:** `admission`
   and `[admission]` (docs/EXECUTION.md, "Admission and the judgment
   band"). The band is recorded, not routed on.
3. Cache-hit ratio as a doctor row and a hold, and a cold-forecast
   input to `continuation=fresh` (2.3). **Built 2026-10-11:** the
   `prompt_cache` doctor watch (docs/OPS.md, row 10) and `[cache]`
   (`handoff::cache_cold`, docs/CONTEXT.md). No hold: a hold on a cost
   ratio could never clear itself, since nothing runs while held, and
   fleet-ops alerts rather than holds too; the row becomes an operator
   alert through doctor-daily like every other watch.
4. Infra-versus-strike classification with a host sample at death, and
   a strong-lane rung before the operator (2.4). **Built 2026-10-11:**
   `engine::step::infra` with `host::Sample` and `[infra]`, and the
   mechanic's `mechanic-strong-lane` with `[mechanic]`
   (docs/WORKFLOWS.md, "Mechanic" and "Infra failures"). Not built:
   fleet-ops' replayed real overload payloads; Forge has none checked
   in yet, so the classifier's patterns are from the research, not from
   a recorded death.
5. Risky-path hold with a non-same-family verdict instead of refusal
   (2.5), which unblocks Forge landing its own prompt and config changes.
   **Built 2026-10-13:** `src/risky.rs`, `[verify] risky` and
   `risky_lines` in the repository, `[risky]` in the operator's config
   (docs/WORKFLOWS.md, "Risky paths"). The verdict is Jev on the
   review's prose; every other case holds for `forge land`. Not built:
   the second reviewer of another family for guard paths, and the
   hold-job-that-never-checks-out shape, which Forge's land step does
   not need since the branch is already verified when it holds.
6. Deploy receipt before closure (2.6), lessons-to-guards ledger (2.7),
   miss-review classes with the twice-rule (2.8). **2.8 built
   2026-10-13:** `src/supervisor/miss.rs`, a `miss-review` decision on
   every mechanic hand-off with a cause key, and the twice rule filing
   one `miss-review-fix` task per key per week (docs/WORKFLOWS.md, "Miss
   review"). 2.6 and 2.7 remain.

## 6. Not verified

- The OP's "200+ commits per day, 10 to 50k lines per day" figures. The
  commit pages read here top out near 80 merges a day.
- Jovie's `lane_runner.py` (387 KB), `canon/OPERATING_SYSTEM.md`, and
  the back half of `scripts/lanes/README.md` (model-summarised).
- fleet-ops' vault of standing rules, which its AGENTS.md names as the
  canonical source; everything above is from the repository only.
- The airc repository (continuum's merger daemon and queue-card CLI).
- griffinwork40/software-factory (agent-afk's intake plugin).
- Whether evener's and continuum's non-author review happens on GitHub
  or only in chat; the PR lists carried no review data.

## 7. Sources

Thread: reddit.com/r/ClaudeAI/comments/1x0ztjs (read via the
arctic-shift archive; reddit.com itself is blocked for the fetch tools
and the browser extension here; the live page source, pasted on
2026-10-11, carried the same eight comments and nothing the archive had
missed). Repositories: Maurdekye/orgtree,
JovieInc/Jovie, Nishfleet/0509, Nishfleet/fleet-ops,
prime-radiant-inc/evener, griffinwork40/agent-afk,
CambrianTech/continuum, all at their 2026-10-11 tips.
