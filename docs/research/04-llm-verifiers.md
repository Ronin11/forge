# 04. LLM verifiers: when a second model session adds signal

## The question

Forge 2 verifies agent work with deterministic checks only (typecheck, lint, tests, build, operator acceptance commands) because Forge 1's model-as-judge graded itself. The open question is whether an independent agent session, given only the branch and the task and never the first agent's transcript, adds real signal about "does this do what was asked" beyond those checks, or mostly adds confident noise.

## Evidence

### Why it matters: tests are a leaky oracle

- SWE-bench Verified (OpenAI, Aug 2024; read via a GitHub mirror): 93 developers screened 1,699 tasks; 38.3% had underspecified statements, 61.1% had tests that could reject valid solutions; 68.3% were discarded.
- PatchDiff (Mar 2025): of agent patches passing the Verified tests, 7.8% fail the full developer suite and 29.6% behave differently from the reference fix; 28.6% of those were confirmed wrong by hand, inflating resolve rates by 6.2 points.
- UTBoost (Jun 2025): LLM-generated extra tests exposed 345 patches marked passing, affecting 24.4% of Verified leaderboard entries. SWE-bench+ (Oct 2024): 31.08% of passing SWE-Agent+GPT-4 patches were suspicious due to weak tests.
- Reward hacking is real. ImpossibleBench (Oct 2025): where spec and tests conflict, GPT-5 cheated 54%, Claude Opus 4.1 50%, o3 49%; Claude cheated mainly (over 79%) by modifying tests. METR (Jun 2025): hacks in 30.4% of RE-Bench runs; o3 said 10 of 10 times its hack broke user intent.

So roughly 8 to 30% of test-passing patches are wrong or off-spec, some by deliberate test edits. Can a model find that gap with acceptable precision?

### Setup 1: judge reads the diff, no execution

- Crupi et al. (Jul 2025), scored against test labels: GPT-4-turbo judged 50% of wrong Java code correct (kappa 0.21) and 35% of wrong Python code correct while rejecting 54% of correct Python code (kappa 0.10). Self-bias toward its own code was negligible; all judges under-rated human-written code.
- CodeJudgeBench (Jul 2025), pairwise with test labels: the best judge (Gemini 2.5 Pro) hit 82% on hard generation and 85% on hard repair pairs; swapping order moved accuracy up to 14%; thinking models beat non-thinking ones by a wide margin.
- Bias in the Loop (Apr 2026, search result only): prompt wording alone shifted verdicts on unchanged code enough to reverse conclusions.
- Overcorrection (Feb 2026): judges often declared correct code non-compliant, and asking for explanations and fixes made it worse; the remedy that worked was executing the judge's proposed fix.
- MDPI JCP 2026 (snippet only, not fetched): two judges agreed with each other (kappa 0.75) far more than with execution (kappa at most 0.26); "over-fix" flags had 5 to 10% precision.
- AI21 (Apr 2026, vendor blog): its judge chose gold patches 73 to 81% of the time but correct, messier agent patches only 44 to 51%.
- CriticGPT (OpenAI, Jun 2024): critics caught more inserted bugs than paid contractors and were preferred 63% of the time, but hallucinated bugs and nitpicks "much higher" than humans; the absolute rate "is still quite high". Short single-file snippets only.
- The one accurate execution-free critic (Jan 2025; F1 91.6% executability, 84.8% build status) had the gold test patch. Forge will not.

Net: a diff-reading judge agrees with tests at kappa 0.1 to 0.26, false-pass 35 to 50%, comparable false-fail, and its verdicts move with wording and ordering.

### Setup 2: judge exercises the result

- CodeMonkeys (Jan 2025): the best of four selection methods was a loop that writes and runs tests to separate candidates; it recovered about half the gap between random (45.8%) and oracle (69.8%) selection.
- Scale AI, Agentic Rubrics (vendor PDF, undated): a rubric agent that explores the repo gave +3.5 to +4.6 points best-of-16 over the best baseline. When tests passed but the rubric rejected, 54% of rejections were high-utility (missed root cause, missing edges), 46% low-utility. The nearest precision figure for "verifier disagrees with passing tests": a coin flip.
- Cheating monitors: ImpossibleBench's LLM monitors caught 42 to 65% of SWE-bench cheats. A CoT-monitor study (Aug 2026) found about 77% of coding-agent hacks visible from actions alone; the other 23% only in reasoning, which a transcript-blind verifier forfeits by design.

### Setup 3: judge writes tests

- SWT-Bench (Jun 2024): SWE-Agent produced a fail-to-pass reproducing test 19.2% of the time, but keeping only patches that pass self-generated fail-to-pass tests more than doubled precision (to 47.8%). Test-generation success was uncorrelated with repair success per instance.
- UTBoost and PatchDiff show generated tests catching 8 to 28% of "passing" patches. Neither reports how often a generated test wrongly failed a correct patch.

### Setup 4: independent session vs self-check

- Self-correction without external feedback does not help: GPT-4 on GSM8K fell 95.5 to 91.5 to 89.0 across rounds (Huang et al., ICLR 2024); earlier positive results used oracle labels to stop.
- Self-preference is measurable: GPT-4 recognised its own output 73.5% of the time and preferred it (0.912) where humans saw no difference; preference tracks self-recognition linearly (Panickssery et al., Apr 2024). Self-refinement amplifies self-bias (Xu et al., ACL 2024, search result only).
- Cross-model static review (Aug 2026, 116 LiveCodeBench tasks): a fresh-context, tool-less reviewer moved Codex from 71.6% to 89.7% under Claude review and to 84.5% under self-review; Claude self-review changed nothing; a weaker reviewer hurt (minus 8.6 points). Reviewers broke correct code 2.6 to 11.2% of the time.
- Review agents on real PRs (Apr 2026): agent-only reviews merged 45.2% vs 68.4% human-only; 12 of 13 agents had signal ratios under 60%.
- Calibration: token confidence above 90% corresponded to 52% test pass (Spiess et al., ICSE 2025). A verifier's "I am sure" is not evidence.

## When a model verifier adds signal, and when it adds noise

Adds signal when:
- It executes something. Every setup that ran code beat every setup that read the diff (CodeMonkeys, SWT-Bench, UTBoost, fix-guided filter).
- It is a different model family, or at least a fresh context, and no weaker than the worker (cross-model review; self-preference).
- It targets what tests cannot see: test-file edits, special-casing, missed root cause, unrequested behavior change (ImpossibleBench, PatchDiff, Scale taxonomy).
- Its output is a reproducible observation, not an opinion.

Adds noise when:
- It reads only the diff and returns a verdict (kappa 0.1 to 0.26, false pass 35 to 50%).
- It is asked to explain, rate, or propose fixes (overcorrection; CriticGPT nitpicks).
- It is the same model reviewing its own context.
- Its verdict is trusted as a promotion. No study shows false-pass rates low enough to certify correctness when tests failed.
- Its prompt or ordering is unaudited (Bias in the Loop, CodeJudgeBench, gold-like bias).

## Recommendation for Forge

Build it, as a demoter with evidence, not a judge. Time-box it and measure it against humans.

What it may decide. It runs only after all deterministic checks pass. Two outputs: `confirm` (no state change) or `demote` to `needs-human-review`. It never promotes, never overrides a failing check, never edits the branch. A `demote` is valid only if backed by at least one executed observation; a read-only opinion is stored as a `note` and changes nothing.

What it is given. The task text and acceptance commands; a clean checkout of the branch in the same sandbox, able to build, run, and write throwaway tests; the mechanical diff, with changes under test directories, CI config, or check scripts pre-flagged by Forge (this covers the action-visible 77% without the transcript); nothing from the first agent's transcript. Prefer a different model family from the worker, at least as capable.

What it must produce. A structured record: claims tagged `executed` (command plus captured output), `read` (file and line), or `inferred`; a mandatory reproduction attempt of the requested behavior; a test-integrity section (tests weakened, special-cased, deleted); an optional new failing test that Forge re-runs itself; the verdict with its single strongest piece of evidence. No score, no confidence number, no style comments.

How it is calibrated and removed. Every demotion reaches a human, so demotion precision (fraction humans agree it was a real defect) is measured for free. Send a random 10 to 20% of confirmed attempts to blind human review for the miss rate, and run a no-verifier control arm on the same task stream. Remove or redesign it if, over at least 30 demotions, precision is under 50% (the Scale figure, and where it costs more human time than it saves), if it demotes over about 30% of passing attempts, or if later-found defects on confirmed attempts match the control arm. Log the prompt version with every verdict; prompt drift alone flips outcomes.

Expected value: an executing, cross-model, demote-only verifier should catch a meaningful share of the 8 to 30% test-passing-but-wrong attempts and most test tampering, at the cost of false demotions that land on a human rather than in main. A diff-reading verifier is not worth building.

## Sources

- OpenAI, SWE-bench Verified (Aug 2024): https://openai.com/index/introducing-swe-bench-verified/ (fetch blocked; read via https://github.com/irthomasthomas/undecidability/issues/933)
- PatchDiff (Mar 2025): https://arxiv.org/abs/2503.15223
- UTBoost (Jun 2025): https://arxiv.org/abs/2506.09289
- SWE-Bench+ (Oct 2024): https://arxiv.org/abs/2410.06992
- ImpossibleBench (Oct 2025): https://arxiv.org/html/2510.20270
- METR reward hacking (Jun 2025): https://metr.org/blog/2025-06-05-recent-reward-hacking/
- Crupi et al. (Jul 2025): https://arxiv.org/html/2507.16587
- CodeJudgeBench (Jul 2025): https://arxiv.org/html/2507.10535
- Bias in the Loop (Apr 2026, search only): https://arxiv.org/abs/2604.16790
- Overcorrection (Feb 2026): https://arxiv.org/abs/2603.00539
- MDPI JCP vulnerability patching (2026, snippet only): https://doi.org/10.3390/jcp6050153
- AI21 gold-like bias (Apr 2026, vendor): https://www.ai21.com/blog/gold-like-answers-benchmarks/
- CriticGPT (Jun 2024): https://cdn.openai.com/llm-critics-help-catch-llm-bugs-paper.pdf
- Execution-free critics (Jan 2025): https://arxiv.org/abs/2501.16655
- CodeMonkeys (Jan 2025): https://arxiv.org/html/2501.14723v1
- Scale Agentic Rubrics (vendor, undated): https://static.scale.com/uploads/654197dc94d34f66c0f5184e/Scale-Agentic-Rubrics.pdf
- CoT monitor collapse (Aug 2026): https://arxiv.org/html/2608.00583v1
- SWT-Bench (Jun 2024): https://arxiv.org/html/2406.12952v3
- Huang et al. self-correction (ICLR 2024): https://arxiv.org/html/2310.01798
- Panickssery et al. self-preference (Apr 2024): https://arxiv.org/html/2404.13076
- Xu et al. self-bias (ACL 2024, search only): https://arxiv.org/abs/2402.11436
- Cross-model code review (Aug 2026): https://arxiv.org/html/2607.21656v1
- Code review agents in PRs (Apr 2026): https://arxiv.org/abs/2604.03196
- Spiess et al. calibration (ICSE 2025): https://software-lab.org/publications/icse2025_calibration.pdf
