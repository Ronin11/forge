# Benchmarks

`forge bench run NAME` submits `bench/specs/NAME.md` as a plan-mode root
against a throwaway repository, lets the recursive loop (plan → batch →
supervise → revise) build it, and records the run's rollup — supervisor
score, cost, wall time — as one row of that benchmark's history
(`forge bench list NAME`). The benchmark is the eval: quality at a fixed
budget should trend up as the library and knowledge base accrete.

A spec is minimal YAML frontmatter over a prose objective. Fields and
defaults (see `internal/tui/cmd_bench.go`):

| field       | default | meaning                                   |
|-------------|---------|-------------------------------------------|
| `size`      | `L`     | S / M / L                                 |
| `model`     | `sonnet`| model alias for the plan; tasks inherit   |
| `autonomy`  | `auto`  | autonomy level                            |
| `max_turns` | `40`    | plan-attempt turns                        |
| `timeout`   | `3600`  | plan-attempt seconds                      |

Every objective is written the same way: a vivid user-value goal, explicit
hard constraints (a `forge.toml` `[checks]` table that is configured and
green is always one of them), and a demo-ready bar. Bench repos are built
in a sandbox with **no network egress**, so every spec must be hermetic:
anything the build needs — data, pages, fixtures — the agent generates
first and commits.

## Specs

| spec | domain | what it exercises / failure classes it catches |
|------|--------|-----------------------------------------------|
| `rebuild-equitizr` | offline web app over a curated ownership graph | Data curation under an honesty rule (mark "reported" rather than invent precision); preserving a specified scoring model exactly; static front-end polish and mobile layout; drilling from claim to evidence; declaring and keeping checks green on a multi-file build. Catches: fabricated seed data, silently altered confidence formulas, "works on my machine" builds with no checks. |
| `scrape-analyze` | resilient HTML scraping over a self-generated hostile corpus | Generating a deterministic fixture corpus *and* its ground-truth manifest before parsing it; parser robustness to malformed markup, encoding, truncation and junk pages; normalisation and dedupe by a documented rule; provenance on every field; treating unparseable input as a reported result rather than an exception or a padded record; wiring an accuracy script (count, per-field accuracy, zero fabrication, spot checks) into `[checks]`; stdlib-first because nothing can be installed. Catches: agents that fetch the network, silently drop or invent records, assert accuracy in prose without a script, or crash on the first bad page. |
| `analyze-dataset` | reproducible causal analysis of a seeded synthetic dataset | Writing down a generating process with a planted effect, a confounder that biases the naive estimate, and a decoy null; cleaning with counted exclusions; choosing and justifying an adjustment method; reporting a defensible interval; producing a short report with charts under environment constraints (SVG if no plotting library); a from-scratch reproduction check, schema/row-count assertions, a recovery check against ground truth the analysis is forbidden to read. Catches: reporting the naive number, chasing the decoy, unseeded or irreproducible pipelines, notebook-dump reports, checks that fail on a missing import, and leaking the answer key into the analysis. |
