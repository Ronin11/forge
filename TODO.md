# TODO

Open items only; completed work lives in git history. (The 2026-09-01 10×
greenfield stress-run postmortem that used to fill this file: 10/10 apps
demo-ready, 0/10 initially verified, every failure Forge's own — all of its
P0/P1 fixes have since landed.)

## P1 — remote access (tomorrow morning, 2026-09-06)
- [ ] Tailscale: `sudo pacman -S tailscale && sudo systemctl enable --now
      tailscaled && sudo tailscale up --operator=ronin` → auth URL on phone;
      then Claude runs `tailscale serve` to publish the UI at
      https://omarchy.<tailnet>.ts.net (daemon stays bound to 127.0.0.1;
      nothing public). Phone gets the Tailscale app on the same account.

## P1 — integrator observability

- [ ] `merges` rows store no check output: when the merge gate fails a check,
  the operator gets `check_failed:lint` and nothing else, and must reproduce
  by hand (scratch clone → detach head → rebase → run the check). Persist at
  least the failing check's tail (like attempts do) on the merge row and show
  it in `task show` / the web UI. Found 2026-09-02 diagnosing equitizr's
  with-deps scratch-clone bug — three verify cycles were burned on a failure
  whose output was never visible.
- [ ] Doctor check for "local base ahead of origin" (from the stale-base
  overnight incident): forge worktrees base on `refs/remotes/origin/<base>`,
  so unpushed local commits are invisible to every agent.

## P2 — housekeeping

- [ ] All 10 demo repos carry `.forge/config.toml` (checks + `[run]`);
  remember this file **shadows** `forge.toml`, so future edits to checks must
  go there.
- [ ] Minor data-cleanup nits from the greenfield audits (non-blocking):
  ownership-card stores a `weight` field the code never reads (one drifted
  value, e0137); neighborhood-report-card has ~19 "scraped" edges whose
  publisher label doesn't match the cited SEC EDGAR URL;
  follow-the-money/guess-the-owner never exercise the `scraped` 0.7 tier.

## P2 — explore repo-specific prompts

- [ ] Repo-scoped prompt fragments (a `.forge/prompts/` per repo, or repo
  sections in personas) could carry per-repo taste the way `.forge/notes`
  carries per-repo learnings — but composition is per-Work while `{{repo}}`
  resolves per-target at claim, so repo-conditional prompt text would push
  composition to claim time and complicate the snapshot/manifest story.
  Explore whether the value beats CLAUDE.md (which the executor already reads
  in-worktree, for free) before building anything.

## P2 — investigate: richer nested data on the Prompts page

- [ ] The library tree flattens structure the data actually has: fragment
  folders render one level deep (`Folder` = full dirname as a single label,
  no collapsing, no recursion), persona mode sections and frontmatter are
  split server-side but the page only surfaces them as chips, and the
  composition manifest is a flat name list with no way to jump from a
  manifest entry to its fragment. If deep trees (fragments/a/b/c.md), more
  frontmatter keys, or per-include drill-down show up in practice, the page
  wants a real recursive tree component and a structured (not chip-flattened)
  detail model — decide then whether that stays hand-rolled or the tree
  becomes a shared partial with the workflow editor's palette.

## Directives follow-ups (2026-09-04 restructure)

Done since: the library IS a registered repo (bootstrap, self-origin —
scratch promotion curation Works run against it), `[directives]` is the TOML
key, routines are target-only, and the 2026-09-04 wipe retired the 29
migrated production directives (the good ones — the flow pipeline,
architecture-docs — were curated into the base first; the old ~/.forge is
archived at ~/.forge.pre-wipe-2026-09-04).

- [ ] A reflection routine targeting the `directives` repo with
  integrate=true — prompt improvement through the ordinary merge pipeline
  (the repo registration it needed now exists).
- [ ] Run-scoped SSE for the run view (the poller swap is ~20 lines).
- [ ] Workflow experiments: third `experimentSubject` implementation
  (`workflow:<name>`) where a variant is a graph edit and a run is a real
  workflow run — the store/pipeline/API are already generic
  (internal/web/experiments.go); the open questions are what "variant" means
  for a graph and how to judge a multi-step run's output.
- [ ] Scratch promotion follow-through: watch the first real promote-scratch
  Works land (auto-merge is on); if curators mangle headers or dedupe badly,
  the directive is the knob. Near-duplicate SCRATCH rows (different names,
  same functionality) still accumulate below the threshold — a periodic
  curation sweep over the cache could merge them.

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

## P2 — verify the loop holds for data work (2026-09-05)

- [ ] Data-scraping bench spec: build a scraper + curated dataset from an
      allowlisted source, with declared checks that validate the DATA
      (schema, row counts, dedupe, freshness stamps) — not just the code.
      Open questions this must answer: per-repo network egress policy (the
      sandbox denies by default; scraping needs scoped allow_hosts, which is
      a worker/config surface, not a prompt), long-poll jobs vs. the attempt
      timeout envelope, and whether the merge queue behaves with large/
      generated files (git-for-data caveats — maybe artifacts, not commits).
- [ ] Data-science bench spec: an analysis with VERIFIABLE claims — L1 =
      a validation suite over the outputs, L2 = an independent agent re-runs
      the pipeline and checks the numbers match the report. The verification
      ideology transfers exactly ("the agent said the correlation is 0.7" is
      not a state); what's untested is whether supervise can judge analysis
      quality vs. shippable-code quality with the same 1-5 rubric.

## P2 — our own execution harness (2026-09-05)

- [ ] Tonight's linger wedge is the case study: the claude CLI is an opaque
      subprocess — telemetry retry loops we can't disable, version-skew
      handshakes, exit behavior we work around from the outside
      (resultLingerGrace). A first-party executor — the Agent SDK / API
      tool-runner driving the loop directly — would own the turn loop,
      retries, context assembly, and shutdown, and could stream structured
      events natively instead of parsing stream-json. The seam already
      exists and is proven: Executors config is pluggable and fake-claude
      rides it; capability parity needed = allowed_tools, json_schema,
      resume, steer, effort, max_budget_usd.
      THE ECONOMIC CAVEAT that decides this: the CLI runs on the Max
      subscription; an API harness pays per token. Likely answer is both —
      keep claude-code as the subscription workhorse, add forge-runner for
      the attempts where control matters more than price (supervise,
      verify, benchmark judges) and measure with the cost columns we
      already have. Prerequisite reading: whether the SDK can run against
      subscription auth at all.


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