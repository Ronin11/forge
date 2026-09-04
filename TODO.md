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

- [ ] A reflection routine targeting `~/.forge/directives` with
  integrate=true — prompt improvement through the ordinary merge pipeline.
- [ ] Consider registering the directives dir as a first-class repo so
  agents can be tasked against it directly.
- [ ] Run-scoped SSE for the run view (the poller swap is ~20 lines).
- [ ] Workflow experiments: third `experimentSubject` implementation
  (`workflow:<name>`) where a variant is a graph edit and a run is a real
  workflow run — the store/pipeline/API are already generic
  (internal/web/experiments.go); the open questions are what "variant" means
  for a graph and how to judge a multi-step run's output.
- [ ] Tighten routine create/update to require `target` once the test
  fixtures migrate off inline content (deviation noted in the P7 commit;
  the store stays dual-mode either way for snapshot/A-B decode).
- [ ] Migrated production directives could name personas the split preserved
  as `persona:` frontmatter — review the 29 split files and consolidate
  duplicated role text against the new base directives (plan-project,
  implement-task, review-change, …).
- [ ] Rename the `[prompts]` config TOML key to `[directives]` with a
  read-fallback.
