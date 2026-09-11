# TODO

Open items only; completed work lives in git history. Cleaned 2026-09-06 —
struck this week's finished work (Tailscale serve, merge-gate evidence,
stale-base doctor check, data-scraping/data-science bench specs + nightly
routines, the directives-repo reflection routine) and deduped the
voice-pipeline section that was pasted twice.

## Waiting on a human decision

## P1 — anti-fragility: failures end as questions, not autopsies
- [ ] (QUEUED as forge task) rate_limited first-class: CLI limit-hit ->
      FailureReason rate_limited carrying provider resets_at -> sweeper
      auto-requeues after the declared reset. Closes the silent-stall gap
      found in the 2026-09-07 stress test.
(Nate, 2026-09-07, after the crashbyforge night): every terminal failure
must resolve to (a) auto-fixed/retried, or (b) a crisp actionable human
question with resume-on-answer — never a state someone must diagnose.
- [ ] Human-actionable failure causes become resumable questions: map
      recognizable errors (Namecheap INSUFFICIENTFUNDS, missing plugin
      config, auth expiry) to an auto-filed question ("top up the balance,
      answer 'funded' to retry") that parks the target waiting_human and
      retries on answer — instead of failing terminally.
- [ ] Plugin self-serve config asks: a plugin missing operator data should
      let the agent collect NON-SECRET values via forge_ask and write its
      own config (e.g. namecheap_registrant_set) — addresses/phones may
      flow through the question channel; only credentials stay config-only.
- [ ] Phantom merges: five agents wrote files, never committed, and the
      chain read "merged" — L0 tolerates dirt (by design), L1 passed, and
      the integrator fast-forwarded zero commits (before==after) as a
      success. Fix at the integrator: before==after is "nothing to merge"
      → unverified, not merged. Also worth a nudge in run-mode prompts:
      uncommitted work does not exist.
- [ ] Retry-after-fix reminder (KB: halted-on-tool-bug note): doctor or
      reflect flags settled tasks that blamed a tool whose plugin binary or
      config changed since — suggest task retry.

## P1 — tooling/platform: the tool ladder and a platform ask channel (2026-09-08 discussion)
Evidence (attempt_facts since 2026-09-04): 417 attempts, 48 wrote scripts/ or
checks/ INSIDE the project, 0 calls to forge_scratch or forge_script_run,
scratch cache empty, 3 library scripts (two toys). Article 11 works at the
project level; org-level tooling that compounds across cycles is at zero.
Root causes: (1) the engineering-standards determinism paragraph steers
agents to repo-local scripts and never mentions the library, scratch, or
searching first; (2) no role's output is tooling; (3) no problems channel —
proposals carry solutions, and an agent mid-task knows the problem better.
Doctrine (Nate): universal tools first, repo-specific second; on a failure
in a new env, log it, then reach for the repo tool; before building a new
universal tool, check every other repo for common ground; a tool's
ascendancy requires docs + tests because it is live code. Daemon stays Go
(plugin/tool boundaries are already language-neutral); WASM via wazero is
the eventual universal-script tier, contingent, not a prerequisite.

Phase 0 — on-ramp (gate: NIGHTSHIFT CRITICAL, scratch non-js runs
unsandboxed on the daemon host; fix BEFORE driving traffic to scratch)
- [ ] Non-js scratch executes in the attempt's worker sandbox, or scratch is
      js-only until it does.
- [ ] Rewrite the determinism paragraph into the ladder: forge_library
      search first → pure-function-of-input goes through forge_scratch →
      repo-local only when it needs the repo. Tool failure in a new env →
      kb note tagged tool-failure scoped to the repo, then fall back.
- [ ] forge_library miss says so and points at forge_scratch; record the
      miss as a fact (query, hit count) — recurrence signal #1.
- [ ] Live experiment on bench roots: control fragment vs ladder fragment;
      outcomes = scratch/script_run call counts + scores. Zero scratch calls
      under the ladder arm ⇒ the sandbox shape (JSON in/out, PATH-only, 30s)
      is the barrier, not the prose.

Phase 1 — trust gates
- [ ] `examples:` header key ({input, output} list) run by the library
      check and POST /script-test; `tool: true` refused at load without ≥1
      passing example. `docs:` block rendered on the Directives page.
      promote-scratch gains both requirements.
- [ ] Per-script adoption in forge_stats: calls, error rate (Wilson), last
      called — from tool_calls_by_name / tool_errors already in facts.

Phase 2 — cross-repo index
- [ ] Schedule-tick job walks every registered repo's scripts/, checks/,
      tools/ and fingerprints files (normalized name, language, header
      comment) into a repo_scripts table; bench repos included.
- [ ] Library search merges hits as kind repo-script labeled by repo; a
      shape in ≥2 repos is flagged shared.
- [ ] Retro pack gains a tooling section: shared shapes, clustered search
      misses, tool failures by repo.

Phase 3 — the ask channel + toolsmith (only when phase 2 shows recurrence)
- [ ] `forge_request`: a typed, non-blocking platform ask — what was
      needed, WHAT THE AGENT DID INSTEAD (mandatory; the anti-stop-early
      guard and the spec for the fix), cost in turns/time, blocked?. Filing
      searches open requests first; a match JOINS and increments (duplicates
      are votes). Weight = distinct attempts/repos/personas × reported cost.
- [ ] `friction` field on run + supervise result schemas (one line, every
      attempt); supervise aggregates across the batch and files a request on
      the team's behalf when a line recurs. Search misses and tool failures
      auto-join requests so machine and agent signals rank together.
- [ ] Routing by kind: tool/script → curate-tools; directive/persona →
      reflect-library evidence; workflow → learning-director; platform code →
      triage Work on the forge repo behind the human approval gate.
- [ ] Threshold → Work (scratch-promotion shape). Deliverable names the
      request it fulfills; adoption stats close the loop — fulfilled but
      unused for weeks reopens with a "solution missed" note.
- [ ] `toolsmith` persona (universal first, rule of three, tests+docs ARE the
      deliverable, never speculative) + `curate-tools` directive on the
      reflect-library skeleton (evidence pass → ≤2 predicted proposals
      against the directives repo: lift / extend / fix failing universal /
      no-op). promote-scratch becomes one path of it. Weekly routine,
      created disabled. Request queue visible on the Learning page with
      human weight/answer/attach.
- [ ] Measure with the bench: quality at fixed budget, tokens_to_first_edit,
      scratch/script_run calls per root.

## P1 — near-term hardening
- [ ] HTTPS on tailscale serve: needs cert enablement on the tailnet
      (`tailscale serve --bg --https=443 localhost:7340` once certs are on);
      today it's plain HTTP inside the tailnet.
- [ ] Family multi-sender activation: signal plugin already takes a
      recipients list with sender-scoped sessions; add family numbers when
      ready. The cloud/family plan is docs/FAMILY.md.

## P1 — graceful redeploy: quiesce, don't kill (2026-09-08)

Operator direction: "if we can track all open processes, do the redeploy, and
resync them, then as soon as they hit the next step, they are updated." Raised
to P1 the same day: three redeploys during the local-runner work each landed on
an idle worker and cost nothing, which was luck, not design — the same three
restarts against a busy worker would have destroyed in-flight attempts silently.
This gates how freely anything else here can ship.

Today's behaviour is self-defeating: both user units set `KillMode=process`, so
systemd stops ONLY the main process and every attempt child (bwrap → claude)
survives the restart — and then the new worker's reconcileOne finds each orphan
by its manifest, confirms ProcessAlive, and KILLS it ("killing orphaned agent
process", reconcile.go:87). We pay to keep them alive across the restart and
then throw the work away.

The constraint that rules out the most ambitious version: forge is not IN a
running attempt. The claude subprocess contains no forge code — forge launches
it, parses its stream-json stdout, supervises, verifies. So "updating" an
in-flight attempt means nothing; what has to survive a redeploy is forge's
RELATIONSHIP to it, which is a pipe.

- [ ] A. Quiesce (do this one). Worker stops claiming new attempts, finishes
      what it holds, exits; systemd restarts on the new binary; the next
      attempt runs new code. That is exactly "updated at the next step", where
      the step is the attempt boundary. Pieces already exist: `slots` is a
      buffered channel (runner.go:204) so in-flight is len(w.slots), and
      registration already reports Active/MaxConcurrent. Needed: a quiescing
      state the daemon honours in the claim path, and a `forge daemon redeploy`
      that sequences build → quiesce → restart → verify. Must be bounded — an
      attempt may run to hard_ceiling_turns=200.
- [ ] C. Resume as the quiesce deadline's fallback, not a separate feature.
      CapResume + SessionID + `--resume` already exist and are proven; if the
      drain does not finish in N minutes, kill and resume after restart rather
      than waiting forever. Costs the current turn's tokens, needs almost no
      new machinery.
- [ ] B. Reattach — the real "resync", and possible only because stdout is
      ALREADY mirrored to a file (supervisor.go:176, newOutputMirror, "raw
      stdout+stderr mirror"). A restarted worker could reopen the mirror at a
      recorded byte offset and resume parsing instead of killing the orphan.
      Costs: a parse offset in the manifest, an adopt path in reconcileOne
      beside the kill path, and adopted attempts lose stdin — so no steering
      unless that is brokered through a file or socket too. Do this ONLY if we
      find ourselves redeploying mid-attempt often; it is the version that
      sounds best and costs the most.
- [ ] The daemon half is much easier than the worker half and could ship
      first: the worker's API client already retries, so a daemon restart is
      largely transparent. The worker is where process state lives.
- [ ] While here: the `KillMode=process` + kill-on-reconcile combination should
      be made coherent whichever way this lands. Either children are meant to
      survive (then adopt them) or they are not (then let systemd reap the
      cgroup and drop the orphan-killing path).

## P2 — headroom trial (low priority, operator-acked 2026-09-07)
- [ ] github.com/headroomlabs-ai/headroom — local context compression
      (20-60% input reduction, proxy mode, KV-cache aligned). Gate 1: does
      the claude CLI's subscription auth tolerate a proxy base URL? (ten-
      minute manual test; decides everything). Gate 2 if yes: benched A/B —
      same spec, proxy on/off, compare score + tokens-per-point + cost.
      Exclude verification-evidence paths from compression on principle.

## P2 — learning-system follow-ups
- [ ] Spend thermostat: "err on the side of better at the beginning, then
      taper" — once the prediction ledger has enough resolved history, tie
      the learning budget/ladder ceiling to calibration (well-calibrated,
      improving → keep spending; flat curves → taper) instead of a static
      usd_per_week.
- [ ] Workflows as the first-class reach (operator direction 2026-09-06:
      "reach for workflows more than just directives... recurring ad-hoc
      chains should become standard like scratch directives"):
      a. Bench-through-workflow: teach startBenchRun a workflow root
         (create bench repo, fire e.g. flow-standard with the spec as
         objective, key scoring off the run) so the two orchestration
         styles compete on the same spec. First concrete consumer.
      b. Distillation loop: reflect/director mandates now require it
         (directives d553dda+) — recurring plan-batch shapes get proposed
         as standing workflows WITH the runnable graph in the after body;
         verify applyWorkflow handles a graph-bearing proposal end to end.
      c. Workflow experiments: third `experimentSubject`
         (`workflow:<name>`) where a variant is a graph edit and a run is
         a real workflow run — store/pipeline/API are already generic;
         open questions are what "variant" means for a graph and how to
         judge a multi-step run's output.
- [ ] Scratch promotion follow-through: watch the first real promote-scratch
      Works land (auto-merge is on); if curators mangle headers or dedupe
      badly, the directive is the knob. Near-duplicate SCRATCH rows still
      accumulate below the threshold — a periodic curation sweep could merge
      them.
- [ ] Run-scoped SSE for the run view (the poller swap is ~20 lines).

## P2 — comms automation: the voice pipeline (2026-09-05 discussion)

Goal: automate ~90% of a corporate-comms workflow (emails, send lists) with
the missing 10% BY DESIGN — she is the L3 gate; nothing auto-sends, drafts
land in her Drafts folder for review. Build it WITH her: corpus consent,
and check her employer's stance on AI/data handling before real content flows.

Architecture decided: voice is a PERSONA (library content), applied as a
workflow step, delivered by the email plugin. Core changes: none — this is
the "content in git, mechanisms in core" rule applied to a person's voice.

Ingestion plan (thousands of her sent emails → three artifacts, never
prompt-stuffed):
- [ ] Corpus repo: one-time PST/mbox export; deterministic parser extracts
      ONLY her authored text (strip quoted threads, signatures, footers —
      colleagues' words stay out); one md file per email with frontmatter
      (date, audience type, recipient count, fresh-vs-reply, situation tag).
      Register as a forge repo — drafting Works run against it, so persona/
      exemplars/stats are in the worktree; grep is the retrieval.
- [ ] Statistical profile (script, not agent): sentence-length medians,
      greeting/sign-off tables, contraction & punctuation habits, list-vs-
      prose, subject patterns, reply length by audience. Persona quotes the
      numbers — executable rules, not "warm but concise."
- [ ] Distillation workflow (`voice-ingest`): map-reduce — plan batch reads
      ~40 emails per attempt → style observations → synthesize
      `personas/<her>.md` → HER review of the persona itself.
- [ ] Exemplar library: ~40–60 real emails covering the situation matrix
      (announce/request/decline/apology × exec/all-staff/external), filed by
      category; `voice-pass` gets rules + 2–3 matched exemplars few-shot.
- [ ] Directives: `voice-pass` (terminal workflow node: rewrite draft into
      persona + checklist) and `voice-calibrate` (reflect-library pattern:
      diff pipeline drafts vs. what she actually sent — her edits are free
      labeled data — sharpen persona via merge pipeline).
- [ ] Email plugin extension (plugins/email has IMAP/SMTP already): watch
      intake, file asks as Work through a comms workflow, deliver to Drafts.
      Send lists = a library script/workflow node assembling recipients.
- [ ] Benchmark: hold out ~50 real emails, reconstruct each ask, run the
      pipeline, score draft vs. her real send (judge + edit distance) —
      same bench machinery as rebuild-equitizr. Done = she can't reliably
      pick her own email out of a lineup.

Next concrete step: build parser + profile script + voice-ingest workflow
against a dummy corpus, ready for the day the real export lands.

## P2 — our own execution harness (2026-09-05)
- [ ] The claude CLI is an opaque subprocess — telemetry retry loops we can't
      disable, version-skew handshakes, exit behavior we work around from the
      outside (resultLingerGrace). A first-party executor — the Agent SDK /
      API tool-runner driving the loop directly — would own the turn loop,
      retries, context assembly, and shutdown, and could stream structured
      events natively instead of parsing stream-json. The seam exists and is
      proven: Executors config is pluggable and fake-claude rides it;
      capability parity needed = allowed_tools, json_schema, resume, steer,
      effort, max_budget_usd.
      THE ECONOMIC CAVEAT that decides this: the CLI runs on the Max
      subscription; an API harness pays per token. Likely answer is both —
      keep claude-code as the subscription workhorse, add forge-runner for
      attempts where control matters more than price (supervise, verify,
      benchmark judges) and measure with the cost columns we already have.
      Prerequisite reading: whether the SDK can run against subscription
      auth at all.
      UPDATE 2026-09-08: a local runner now exists (dev.home ollama, billing
      "local"), which changes the economic caveat — a local executor pays no
      per-token price at all, so "control matters more than price" stops being
      a trade. It does not change the build cost: capability parity is still
      the whole job, and qwen3-coder's tool-calling works but is unproven on
      real specs. Settle it with bench rather than argument — spec-for-spec
      against claude-code, which is the one comparison that would justify it.

## P2 — model system: roles, not another scale (2026-09-08)

Operator direction: "I think it needs to be a tad more flexible... I want to be
able to use this local box a bit more... a better model do the prompting and
supervision"; and "maybe we should have like a power level? Then a recommended
level that can help with routing... lets say I think [OpenAI] do a better job of
planning and optimizing, so I want to put my thumb on the scale for those jobs."

The symptom: six separate one-off ways to name a model for a job, no shared
concept — `[attention] model`, `[supervision] decider_model`, experiments'
`target_model`/`optimizer_model`, routines' `model`/`models[]`, workflow nodes'
`DirectiveNodeConfig.Model`, and `assistantModel = "haiku"` which is a Go
CONSTANT in handlers_assistant.go and not configurable at all. Every new role
costs another config field and another plumb-through.

- [ ] A model-role table is the revamp: named roles (decider, assistant,
      optimizer, drafter, planner, judge) → alias, in one place, with
      routine- and node-level override. Kills assistantModel-as-constant,
      makes "move this job to the local box" a config line instead of a code
      change, and gives the Resources page something coherent to render.
- [ ] Do NOT add a "power level" scale. `ModelConfig.Class` (frontier | mid |
      small | local) already IS one, and already drives the
      `modes/<mode>.<class>.md` prompt overlays. Two real problems to fix
      instead of adding a third concept:
      a. `local` is a LOCATION, not a capability — qwen3-coder:30b is ~small
         in capability and local in deployment, and `runner.billing = "local"`
         already encodes the location. Conflating them means a better local
         model (or a cheap hosted small one) cannot be described. Split the
         axes; keep class capability-only.
      b. `model.BudgetClass` (ClassNormal/ClassBacklog) is a DIFFERENT Class
         already in the tree. A third Class-ish noun would be a mess — name
         carefully.
- [ ] The thumb on the scale is per-ROLE, not a level, and this is the crux: a
      scalar can say "gpt5 ≈ opus" but can never say "better at planning, worse
      at coding," which is exactly the preference wanted. Model it as
      `[roles.planner] prefer = ["gpt5","opus"]` (ordered bias) plus
      `min_class = "frontier"` (the "recommended level", as a hard floor).
      Floor filters; preference biases.
- [ ] `prefer` must be a PRIOR, not an override. Routing already has a Wilson
      lower bound on verified success per routine/model, a cost vector and an
      explore probability; a hard override switches off the part that makes
      forge forge. A new subscription has zero samples, so min_samples would
      otherwise leave the router exploring blind — a prior is the right way to
      seed it, and accumulated evidence must be able to overturn it. Operator
      intuition seeds; measurement decides.
- [ ] Explicit composition stays in workflows, not routing.
      `DirectiveNodeConfig.Model` already overrides per node, so "plan on the
      strong model, build on the cheap one, review on the strong one" is
      expressible TODAY. The ladder is an escalation mechanism; composition is
      a different idea and should not get a second mechanism inside routing.
- [ ] Prerequisite before trusting any weak model with a role: there are ZERO
      class overlays in ~/.forge/modes (fifteen base modes, no
      `<mode>.local.md`). Every prompt there was written for frontier models —
      move roles onto a 30B without overlays and you measure the prompts, not
      the model. Cheapest high-leverage win in this section.

### If an OpenAI subscription enters the picture
- [ ] The two purchases are NOT interchangeable and need deciding before
      buying. A ChatGPT *subscription* is not an API credential and will not
      work with the openai-compatible runner modelCall uses; its path is
      `codex exec`, i.e. a SECOND CLI executor — command template plus one new
      output parser — which would also get real attempts, not just decisions.
      codex-cli 0.153.4 is already installed here and exposes `--json`,
      `--output-schema <FILE>`, `-m/--model` and a bypass-approvals flag, which
      map onto TemplateExecutor and the json_schema capability almost exactly;
      the open questions are allowed_tools, resume and steer parity. An OpenAI
      *API key* is the opposite trade: works with modelCall today for zero new
      code, but bills per token (billing = "api", real dollars, unlike the
      subscription's notional cost).

## P2 — resource telemetry: a long-running signal, page on top (2026-09-08)

Operator direction: "we want a long running signal here, especially as we play
with different models and load times... second to minute level data. Dashboard
is a nice front of it."

The motivating failure: dev.home's ollama ran 100% on CPU for three weeks
because its driver (470/CUDA 11.4) was too old for ollama's cuda_v12 runners,
and nothing noticed — it was found by hand while wiring the local runner. A
page alone would not have caught it either; something has to be sampling.

- [ ] Time series, not current state. `runners` (name, kind, billing, capacity,
      endpoint, health, last_probe_at) holds only the newest probe. A signal at
      second-to-minute resolution needs its own table AND a retention/rollup
      policy decided up front ([retention] already has the shape) — 1Hz across
      a handful of gauges is ~86k rows/day/host, and keeping raw forever is not
      an option.
- [ ] Sampling rate picks the collector, and this is the real fork. At the
      current 2-minute probe interval, `ssh dev.home nvidia-smi` polls fine and
      deploys nothing. At 1Hz that is 86k ssh handshakes/day and clearly wrong.
      Two candidates: (a) ONE long-lived ssh streaming `nvidia-smi --query-gpu
      ... -l 1`, supervised and ingested line by line — zero deploy, but needs
      reconnect logic and forge holds the ssh creds; (b) a small exporter on
      the box — robust and conventional, but something to deploy and version.
      If (b), build it as a forge PLUGIN (manifest, process, token, scopes,
      journal all exist already) rather than inventing a second extension
      mechanism beside the plugin system.
- [ ] Two sources, very different costs. ollama's `/api/ps` is plain HTTP to a
      port forge already talks to and carries exactly the field that regressed
      silently (PROCESSOR = "100% GPU" vs "100% CPU"), plus resident model,
      size and context — cheap to sample often, nothing to install. GPU
      utilization / VRAM / temperature / power need nvidia-smi on the host.
      Temperature earns its place: thermal throttling appears as a tok/s cliff
      with no other symptom.
- [ ] Load time and throughput come free, but only from the RIGHT endpoint.
      ollama's native `/api/generate` and `/api/chat` return `load_duration`,
      `prompt_eval_duration`, `eval_duration` and `eval_count` per request —
      exact cold-load cost and tok/s, which is precisely the "load times"
      signal wanted. The OpenAI-compatible `/v1/chat/completions` that
      modelCall uses does NOT return them. So telemetry either calls the native
      endpoint or the client records wall-clock itself; decide before building,
      because it changes model_openai.go.
- [ ] Load-bearing before decorative. probeOpenAI returns
      ready|down|unauthenticated, but a runner that answers while pinned to CPU
      is DEGRADED, not ready. Feed that distinction into doctor (read daily)
      and into routing. This is the anti-fragility rule applied to hardware:
      the three-week regression should have surfaced as a question rather than
      waiting for someone to benchmark it by hand.
- [ ] Only then the page. `system.html` is a 9-line stub titled "Settings"
      (Workers + Repositories) that wants to grow into this, and runnerHealth()
      in ui.go already folds runner:<name> across workers for the dashboard —
      so the health half is largely a view over data forge already has.
- [ ] Scope the subscription half honestly: forge knows five-hour/seven-day
      window percentages and per-attempt cost_usd with a billing class per
      runner, but nothing about renewal dates, plan tier or seats — those would
      be operator-entered config, not live data. Decide whether a half-live
      panel earns its place before building it.
- [ ] Do not conflate with bench. Bench measures task-level quality and cost
      per spec; this measures infrastructure throughput. Different questions,
      and they should not share a scoreboard.

## P2 — housekeeping
- [ ] Windows build: the daemon's process model is POSIX (process groups,
      flock, exec-in-place restart, SIGUSR1, statfs, umask) and does not
      compile for windows/amd64; the site says "WSL2" for now. The file
      list and the shape of a port are in docs/RELEASING.md §Windows; once
      it compiles, add the target to scripts/release.sh and `just cross`.
- [ ] All 10 demo repos carry `.forge/config.toml` (checks + `[run]`);
      remember this file **shadows** `forge.toml`, so future edits to checks
      must go there.
- [ ] Minor data-cleanup nits from the greenfield audits (non-blocking):
      ownership-card stores a `weight` field the code never reads (one
      drifted value, e0137); neighborhood-report-card has ~19 "scraped"
      edges whose publisher label doesn't match the cited SEC EDGAR URL;
      follow-the-money/guess-the-owner never exercise the `scraped` 0.7 tier.
- [ ] Repo-scoped prompt fragments (`.forge/prompts/` per repo, or repo
      sections in personas): explore whether the value beats CLAUDE.md
      (which the executor already reads in-worktree, for free) before
      building — composition is per-Work while `{{repo}}` resolves
      per-target at claim, so repo-conditional prompt text pushes
      composition to claim time and complicates the snapshot/manifest story.
- [ ] Prompts page: if deep fragment trees, more frontmatter keys, or
      per-include drill-down show up in practice, the library tree wants a
      real recursive tree component and a structured (not chip-flattened)
      detail model — decide then whether it stays hand-rolled or becomes a
      shared partial with the workflow editor's palette.

## P3 — interface ideas (jotted 2026-09-06)
- [ ] Companion app: a proper mobile front end — sessions/chat, queue,
      Learning feed, and one-tap Human Queue approvals (the purchase gate on
      a button). Tailscale already carries the transport; today's UI is
      operator-grade, not thumb-grade.
- [ ] Voice mode plugins: speech in/out as a channel class — voice notes over
      the existing bridges and/or a live voice loop; the concierge already
      speaks sender-scoped sessions, so this is transcription + TTS at the
      plugin layer. First slice (Nate, 2026-09-06): a Signal voice note gets
      transcribed to text and executed like any typed message — signal-cli
      receive exposes attachments, so this is STT (local whisper.cpp or API)
      wired into the intake loop.
- [ ] Google Home integration: "Hey Google, ask Forge…" — smart-speaker
      intake/status through the assistant route; approvals stay on
      phone-confirmed channels, never voice-only.
- [ ] Phone number for forge: decide the goal first — specifically Google
      Voice (no provisioning API; browser-executor + credential-broker
      territory, CAPTCHA may cap it at semi-automated) or just "a number
      forge owns" (Twilio/Telnyx provision entirely by API, ~$1/mo + usage —
      an afternoon, not a browser fight).
- [ ] Browser executor + Bitwarden credential broker: the deferred layer
      from the domain-launch work — lets forge drive authenticated web flows
      (Google Voice signup, registrar consoles) with credentials brokered
      per-approval, never in agent context.
