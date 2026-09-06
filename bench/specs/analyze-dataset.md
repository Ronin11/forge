---
size: L
model: sonnet
autonomy: auto
max_turns: 50
timeout: 5400
---
Build "churn-study" from scratch in this empty repository: a demo-ready,
fully reproducible data analysis that ANSWERS ONE QUESTION A
SUBSCRIPTION BUSINESS WOULD ACTUALLY PAY FOR — "does our onboarding call
reduce 90-day churn, and by how much?" — and gets the answer RIGHT even
though the naive comparison is wrong. The value is the discipline: a
dataset whose true generating process is written down, an analysis that
recovers the planted effect with honest uncertainty, a report a
non-statistician can read in five minutes, and checks that prove the
whole thing rebuilds from an empty directory to the same numbers.

HARD SEQUENCING RULE (three benches have now shipped with an empty
[checks] table): the FIRST task of the plan must wire forge.toml's [checks]
with at least a build/typecheck command that runs green on the scaffold,
and every later task keeps it green and extends it. A final product whose
[checks] is empty scores as a failure regardless of anything else.

HARD CONSTRAINTS (this is a demo you could hand a client):

- HERMETIC. This repository is built and checked in a sandbox with NO
  NETWORK EGRESS. No datasets are downloaded and no packages can be
  installed: use whichever mainstream language you choose with what is
  already present (Python with numpy/pandas/matplotlib if they import,
  otherwise the standard library — a hand-rolled bootstrap and
  hand-written SVG charts are perfectly acceptable and count as
  "charts"). Probe what is available first and design around it; a
  report that says "matplotlib was unavailable, charts are SVG" is
  honest, a check that fails on import is not.
- THE DATASET IS SYNTHETIC, SEEDED, AND DOCUMENTED — AND YOU GENERATE IT
  FIRST. Ship a generator (fixed seed recorded in code) that writes
  data/customers.csv with on the order of 20,000 rows and a data/DATA.md
  that states the generating process as equations and rules, not prose
  hand-waving. Plant, at minimum: (1) THE EFFECT — customers who receive
  an onboarding call have their 90-day churn probability reduced by a
  known amount (a true effect on the order of 8 percentage points, the
  exact value your choice); (2) A CONFOUNDER — call assignment depends
  on plan tier and signup channel, and those ALSO drive churn on their
  own, so the raw churn difference between called and not-called
  customers is materially biased away from the truth (make the naive
  estimate wrong by at least 3 points, in a direction you document);
  (3) A DECOY — a variable (e.g. "welcome email variant") with exactly
  ZERO causal effect that a careless analyst might report as
  significant; (4) realistic mess — a few percent missing values in a
  non-outcome column, a small cluster of implausible outliers (negative
  tenure, 999-day usage), and one categorical column with inconsistent
  casing/whitespace. The generator writes data/ground_truth.json with
  the true parameters. The analysis code MUST NOT read that file; it
  exists only for the checks.
- THE ANALYSIS MUST FIND THE EFFECT AND OWN ITS UNCERTAINTY. Clean the
  data with every exclusion counted and justified. Present the naive
  difference and then the adjusted estimate (stratification, regression
  adjustment, matching, or IPW — pick one, name it, explain in one
  paragraph why it removes the planted confounding). Attach a confidence
  interval computed by a method you can defend (bootstrap or analytic),
  and state the assumptions under which it is a causal estimate. Test
  the decoy and report it as null. Write the numbers to
  out/results.json (naive_estimate, adjusted_estimate, ci_low, ci_high,
  method, n_analysed, n_excluded, decoy_estimate, decoy_ci) so machines
  can read what the report claims.
- A SMALL REPORT, NOT A NOTEBOOK DUMP. out/REPORT.md: the question, the
  data in one paragraph, a figure showing raw churn by call status
  against the confounder strata (so the reader SEES the bias), a figure
  of the adjusted effect with its interval next to the naive one, the
  decoy result, limitations, and a plain-language bottom line. Charts
  are files under out/ referenced from the markdown; each has a title,
  labelled axes, and units. Keep it under two screens.
- REPRODUCIBILITY IS A CHECK, NOT A PROMISE. Declare the repository's
  checks in a forge.toml [checks] table at the root and keep them green.
  At minimum: a schema check on data/customers.csv (exact column set and
  types, value ranges, unique customer ids, exact expected row count); a
  regeneration check that runs the generator into a temporary directory
  and fails unless the CSV is byte-identical to the committed one; a
  from-scratch check that deletes out/, re-runs the full analysis, and
  fails unless results.json and every chart are reproduced identically;
  a recovery check that reads data/ground_truth.json and asserts the
  adjusted estimate's interval covers the true effect, the point
  estimate is within a documented tolerance, the naive estimate is
  outside that tolerance (the bias is real), and the decoy's interval
  covers zero; and a guard that greps the analysis code for
  "ground_truth" and fails if it is referenced. Unit tests for the
  cleaning rules and the estimator on a tiny hand-computable example.
- One obvious entry point (`make all` / a single script) that goes from
  nothing to data, results, charts and report, and a README that runs
  the loop in under a minute and tells a reviewer where the truth lives
  and how the checks compare against it.

Deliver the generator with its written-down process, the cleaning and
adjusted analysis with real uncertainty, the report with charts, and
the wired validation and reproduction checks. The bar is "finds the
planted effect, says how sure it is, does not fall for the decoy, and
rebuilds identically on demand" — not model sophistication.
