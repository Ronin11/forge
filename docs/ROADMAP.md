# Forge 1.0: what stable means, and what is left (2026-09-21)

Forge is thirteen days old and has landed 490 tasks on itself and two
products. This page says what "1.0" means for it, marks what is done,
and lists what remains in the order it should go, so the queue can be
filled from here without a conversation. docs/LATER.md keeps the parked
ideas with a status line each; this is the short list.

## What 1.0 means

Not a feature set. A week of the following, measured from the record:

1. **Unattended.** Seven days of landings on every project with no hand
   on the box except answers to questions the ladder raised. Zero hand
   rebuilds, zero hand restarts, zero edits to the store.
2. **Every failure ends somewhere.** A task ends landed, or as one crisp
   question, never in a state that needs diagnosis. Every red job files
   its question. No flake older than a day.
3. **The record is complete.** Everything Forge did is a row: attempts,
   integrations, deploys, jobs, messages, decisions, hand landings. The
   numbers (`stats`, `--quality`, `--by-role`) are computed from those
   rows and nothing else.
4. **Redeploy is nobody's job.** A landing on the Forge repository
   rebuilds and restarts Forge the way a landing on equitizr deploys
   equitizr.
5. **Posture.** No listener without a token, no attempt outside the
   sandbox, sandbox egress bounded, the store backed up off the box,
   secrets never in a prompt or a log.
6. **Docs are tests.** Every claim a doc makes about the tree is held by
   a test (built-ins, verbs, layout, the client contract, the workflow
   format) or the doc says it is a note.

## Done (the week of 2026-09-14 to 21)

- Two engineering loops, docs/REVIEW.md and docs/REVIEW-2.md, every
  stage landed, most of it through Forge.
- Projects, initiatives, deploy targets with smoke and look, intake and
  the concierge, the portal, plugins, three providers and three runners.
- Jobs: the definition, directive steps, the executor with the
  repository's setup, all four triggers, delayed jobs, skipping,
  fixtures and `forge job test`, the human rung, automations in the
  portal. docs/JOBS.md steps 1 to 6.
- Standing checks on a schedule: daily doctor, weekly drift, weekly
  engineering measurement, each keyed so a restart never fires twice.
- Measurements: true cost, repair cost by line, defect escape, churn,
  assessment correlation, human attention (four named signals), time to
  live per workflow and per project, by-role statistics that count job
  steps.
- The worker: signals never lost, re-polls while running, held
  initiatives visible; `forge doctor` covers all of it.
- Equitizr public at equitizr.com with self-deploy, and its own
  automation validated and replayed on every task. Nucleosynthesis with
  its onboarding landed.
- UTC everywhere in the kernel; every client renders local time.

## Open, in order

**Before 1.0**

1. **Self-deploy.** A deploy target on the Forge project: on landing,
   `cargo build --release --workspace`, then restart `forge-worker`,
   `forge-web` and `forge-portal` after the worker drains, with the
   existing smoke check against the web client. Removes every hand
   rebuild of this week and the stale-client bug it caused.
   One directive and one target; the method exists.
2. **The store backup job.** A daily run workflow that copies
   `forge.db` and the operator config to the Hetzner box, asserts the
   copy opens, and keeps seven. One workflow file.
3. **The rate-limit rerun from 97%.** The refusal, the refund and the
   wait are exercised only by fakes. Fuel queued at the end of a weekly
   window; the write-up goes in docs/LATER.md's section.
4. **Sandbox egress. Done (2026-09-21).** An attempt runs in a bwrap
   network namespace whose only route out is an allowlist proxy on a
   unix socket (`src/egress.rs`; HTTP_PROXY and HTTPS_PROXY name a relay
   on 127.0.0.1:3128 in the namespace). It allows the model endpoint of
   every configured provider, always, and the hosts the repository
   declares in forge.toml as `[sandbox] egress = ["registry.npmjs.org",
   "*.crates.io"]`, read from the base so an attempt cannot widen its
   own list; the rest gets a 403 naming the host. The built-in
   `egress-probe` operation asserts a direct connection is refused, the
   proxy answers, and a denied host is refused; `forge doctor` reports
   the policy per project. The github-issues plugin is unheld.
   Cost, measured: the e2e suite, whose fakes are local scripts, took
   32.2 s before and 31.5 s after (233 tests, one run each); the relay
   adds one small process per sandboxed command. A repository whose
   checks install packages must declare the registries (`npm ci` needs
   `registry.npmjs.org`) or keep a warm cache in the operator's
   `rw_paths`, which is how Forge's own cargo checks run today. Left for
   a human, because forge.toml is protected: declare `*.crates.io` in
   Forge's own forge.toml so a new dependency can be fetched.
5. **Bench the `cheap` workflow and raise `changelog-line`'s budget.**
   `cheap` has never run; the changelog job cannot finish on a hosted
   provider under its own cap. Two small tasks that make two numbers
   real.
6. **A `direct` verdict.** `direct` verifies 36% of its tasks against
   `reviewed`'s rate; either the profile says when it is worth choosing
   or it stops being a default anywhere. A reading of the record, then
   one line in docs/ACTIONS.md.

**1.0 itself:** the week described above, measured, written up as the
outcome section of this page.

**After 1.0**

7. **The factory on dev.home, products in containers.** Reinstall the
   VM, join the tailnet with the Proxmox host as a subnet router, move
   the store and the units, then `provision-proxmox` beside
   `provision-hetzner` so a landing can deploy into an LXC. Customer
   names stay on Hetzner. Per-branch previews follow from the same
   operation with a TTL. Your reinstall and API token first, then three
   or four Forge tasks.
8. **Public names.** Nucleosynthesis at a hostname you own, on the
   equitizr box behind Caddy with the same on-landing deploy; the portal
   at `portal.<domain>` proxied over the tailnet to wherever the store
   lives. One decision, then two targets.
9. **The first customer automation** (docs/JOBS.md step 7), from a
   confirmed brief. Starts with the intake interview (task 370).
10. **Context and grounding**, only if the turns-before-first-edit
    number says so: the `history` operation, the graph in the prompt.
11. **The visualiser and the inspector**: the module graph with the
    record overlaid, then the scrubbable run. Reads what exists.
12. **Digital twins** with the first integration-heavy automation.
13. **Measured routing**: roles as a prior the profiles can overrule,
    once enough workflows are measured.

**Retired from the list:** the journal experiment (ill-posed, closed),
the constitution as a document, a network listener without auth, the
resume site (deprioritized by decision), the local model for code
attempts (measured, not viable; it keeps the judgment steps).

## Outcome

To be written after the measured week.
