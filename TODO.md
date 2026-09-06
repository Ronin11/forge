# TODO

Open items only; completed work lives in git history. Cleaned 2026-09-06 —
struck this week's finished work (Tailscale serve, merge-gate evidence,
stale-base doctor check, data-scraping/data-science bench specs + nightly
routines, the directives-repo reflection routine) and deduped the
voice-pipeline section that was pasted twice.

## Waiting on a human decision
- [ ] crashbyforge.com: available at $10.98/yr, Namecheap plugin's purchase
      gate is armed — registers only on Nate's explicit approve via the
      pending question. Then: dns_set → GitHub Pages for the ashbyforge site
      (launch checklist lives in the site repo).
- [ ] Workflow proposal 543c254d (task-integration): held pending a week of
      post-L0-fix evidence — re-review ~2026-09-13 against fresh bench trees.
- [ ] Five backfilled product-review works sit waiting_human (cadence's first
      tick swept a 14-day window; the window is now 36h). They pitch against
      stale bench snapshots — answer or cancel them (`forge task cancel`).

## P1 — near-term hardening
- [ ] HTTPS on tailscale serve: needs cert enablement on the tailnet
      (`tailscale serve --bg --https=443 localhost:7340` once certs are on);
      today it's plain HTTP inside the tailnet.
- [ ] Doctor check for plugin.denied journals — a plugin repeatedly bouncing
      off scopes (like signal's 403 week) should surface in `forge doctor`,
      not in a confused chat.
- [ ] Family multi-sender activation: signal plugin already takes a
      recipients list with sender-scoped sessions; add family numbers when
      ready. The cloud/family plan is docs/FAMILY.md.

## P2 — learning-system follow-ups
- [ ] Spend thermostat: "err on the side of better at the beginning, then
      taper" — once the prediction ledger has enough resolved history, tie
      the learning budget/ladder ceiling to calibration (well-calibrated,
      improving → keep spending; flat curves → taper) instead of a static
      usd_per_week.
- [ ] Workflow experiments: third `experimentSubject` implementation
      (`workflow:<name>`) where a variant is a graph edit and a run is a real
      workflow run — store/pipeline/API are already generic
      (internal/web/experiments.go); open questions are what "variant" means
      for a graph and how to judge a multi-step run's output.
- [ ] Scratch promotion follow-through: watch the first real promote-scratch
      Works land (auto-merge is on); if curators mangle headers or dedupe
      badly, the directive is the knob. Near-duplicate SCRATCH rows still
      accumulate below the threshold — a periodic curation sweep could merge
      them.
- [ ] Run-scoped SSE for the run view (the poller swap is ~20 lines).

## P2 — UI
- [ ] Diagramize the Workflows page (idea 2026-09-06): render each workflow
      graph as a real diagram — mermaid flowchart generated from the graph
      model is the cheap road (client-side mermaid.js, nodes/edges are
      already in the store); the editor's palette could share it. Live runs
      could highlight the active node. Decide mermaid-vs-existing graph.js
      before building — graph.js already draws something; the ask is
      "better", so compare first.

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

## P2 — housekeeping
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
