# Self-Improving System: Cold Design

Written without looking at the repo, DESIGN.md, or the original forge. Baseline for comparison.

## The one rule

The LLM proposes, deterministic code disposes. Every change the system makes to itself is authored by a model but applied, tested, scored, promoted, and rolled back by code that never changes on its own. A self-improving system without a fixed judge just drifts. So the first design decision is which parts are frozen.

## Tiers of mutability

Everything in the system is a versioned artifact with a content hash, and each artifact lives in a tier that decides who can change it and how hard the gate is.

- **Tier 0, frozen kernel.** The experiment ledger, the promotion gate, the sandbox runner, the metric definitions, the rollback command. Humans only. This is the part that makes the rest safe to automate.
- **Tier 1, evaluation suite.** Test cases, graders, held-out sets. The loop may add cases but only through a one-way door. Removing or weakening a case is human-only.
- **Tier 2, behavior.** Prompts, routing policies, tool definitions, skills, generated code. Freely modifiable by the loop, gated by the eval suite and a canary.
- **Tier 3, working memory.** Notes, learned facts, failure summaries. Cheap and continuous, with a size cap and a decay rule.

The tiers matter because the biggest risk is not a bad prompt. It is the system quietly making the test easier.

## The loop

Six stages, alternating between model and code.

1. **Observe, code.** Every production task emits a trace: inputs, artifact hashes used, tool calls, output, cost, latency, and any downstream signal like user correction or a failed check. Traces go in an append-only store. Nothing here is a model.
2. **Diagnose, model.** A model reads recent failures plus the ledger of past experiments and produces a ranked list of hypotheses in a strict schema. It must cite the traces it is reasoning from, and it must say which past experiments already tried something similar. The ledger is what stops it from rediscovering the same idea every night.
3. **Propose, model.** For one hypothesis, a model writes a diff against one artifact. One artifact per experiment, so results are attributable. The output is a patch, never a direct write. The model has no credentials to production state.
4. **Verify, code.** The patch is applied in a sandbox with resource caps and no access to the ledger or gate. The full eval suite runs with fixed seeds and repeated samples, and the result is a distribution, not a single number. A challenger has to beat the champion on the primary metric with a significance test, and it may not regress on any guardrail metric: cost, latency, safety classifier, held-out set. Multi-objective gating is what prevents the loop from trading quality for cost without anyone noticing.
5. **Promote, code.** Winning tier 2 and 3 changes ship to a canary slice of real traffic. Real-world signal outranks the synthetic eval. If the canary holds for a cooldown window, the challenger becomes the champion. Anything touching tier 1 goes to a human review queue instead.
6. **Record, code.** Hypothesis, diff, eval results, canary outcome, and the decision go into the ledger. Failures are recorded as carefully as wins. A model then periodically distills the ledger into a tier 3 "what we've learned" note that the diagnoser reads next round.

## Where the LLM earns its place

Three jobs that deterministic code is bad at.

- **Failure clustering.** Grouping a thousand traces into eight actual problems.
- **Adversarial case generation.** Hunting for inputs the current champion fails on. Each candidate is verified by code to actually fail, then a second model or a human confirms it is a legitimate case before it enters tier 1. In practice this is the highest-leverage part of the loop. Most systems are limited by their tests, not their prompts.
- **Writing the change.** Prompts, tool wrappers, code.

Everything the model writes crosses the boundary as validated structured output. If it does not parse or violates the schema, the stage fails closed and is logged as a failure of the proposer, which is itself a metric.

## Defenses against fooling itself

- **Held-out evals** that the proposer never sees, rotated on a schedule.
- **Frozen graders.** If a grader is a model, its prompt and model version are pinned per eval-suite version so scores stay comparable across time.
- **One change at a time per artifact**, with a cooldown before that artifact can change again, so real-world signal has time to accumulate.
- **Rollback is a single deterministic operation** that only needs a hash. It is tested more often than anything else.
- **A meta-metric on the improver.** Promotion rate, post-promotion regression rate, and cost per promoted change. If post-promotion regressions rise, code tightens the significance threshold. The thresholds are adjusted by fixed rules, not by a model, because that is exactly where a model would loosen them.

## Build order

1. Tracing and the ledger. Boring and mandatory.
2. Eval harness with a champion, running in CI, humans making all changes.
3. Swap the human diagnoser for a model. Humans still promote.
4. Auto-promote tier 3, then tier 2 with canary.
5. Adversarial eval generation into tier 1 with human review.
6. Only then, let the system generate code artifacts, since that is where the blast radius is largest.

The short version is that "self-improving" is really "a normal experimentation platform where the experimenter happens to be a model." Most of the engineering is the platform, and almost none of it is the model.
