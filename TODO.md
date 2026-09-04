# TODO

Open items only; completed work lives in git history. (The 2026-09-01 10×
greenfield stress-run postmortem that used to fill this file: 10/10 apps
demo-ready, 0/10 initially verified, every failure Forge's own — all of its
P0/P1 fixes have since landed.)

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

## Workflow/persona follow-ups (2026-09-04 revamp)

- [ ] Point the standing routines at the starter personas (review →
  senior-reviewer, arch-docs → docs-writer, …) and shrink their prompts to
  task text.
- [ ] A reflection routine targeting `~/.forge/prompts` with integrate=true —
  prompt improvement through the ordinary merge pipeline.
- [ ] Consider registering the prompts dir as a first-class repo so agents
  can be tasked against it directly.
- [ ] Run-scoped SSE for the run view (the poller swap is ~20 lines).
- [ ] Workflow nodes without routines (persona + mode + objective directly)
  once personas prove out.
