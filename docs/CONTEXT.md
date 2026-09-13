# Context: what the coder is told before it starts looking

Written 2026-09-13, after reading Uber's software-factory post and a
survey of how aider, Cody, Copilot, Claude Code, and Cursor supply
repository context, against our own attempt logs.

## What our logs say

Across 62 code attempts and 38 test-author attempts on nucleosynthesis
(medians):

| step | tool calls | before first edit | read/grep before first edit |
|---|---|---|---|
| code, succeeded | 25 | 9 | 2 |
| code, hit the turn cap | 34 | 14 | 7 |
| tests, all | 37 | 16 | 10 |
| tests, hit the turn cap | 46 | 17 | 11 |

Capped attempts explored twice as much as successful ones before touching
anything; the test author explores hardest. And the reads are the same
files every time: types, data, state, tick, save, index, bot, and the
protected progression test account for most reads across every attempt.
Nothing is re-read within an attempt. The waste is that every attempt
starts blind and rediscovers the same eight files, and every retry is a
fresh session with a cold cache.

## What the literature says (sources in the survey)

- Static repository overviews (AGENTS.md-style, machine-generated or
  hand-written) do not raise success rates and cost ~20% more tokens.
  What helps is the non-default: conventions, pitfalls, cost warnings
  ("the suite takes 40 s, run one file").
- Short, task-conditioned summaries of *prior related work* (~200
  tokens) raised resolution from 26% to 34% and cut cost; agents
  retrieving the same material themselves cost 27% more with no gain.
- Seeding the agent with the right files shortens exploration by about
  one step; oracle seeds far more. Symbol maps and embeddings retrieve
  different things; fusing them beats either. Budgets are small
  everywhere (aider ~1k tokens) and rankings flip with the budget.
- Missing context is the dominant failure mode; redundant context hurts
  less. Agentic exploration still beats one-shot retrieval on line-level
  recall, so the aim is a better start, not a replacement.

## Architectures, and what each would be here

1. **A static map in the prompt.** `docs/SYSTEM.md`, which the `graph`
   directive already keeps. Cheapest to try; the evidence says it will
   save a few turns and not change success. Worth measuring, not worth
   betting on.

2. **A task-conditioned repo map.** aider's shape: symbols per file,
   ranked against the task text, cut to a budget. Here it is an
   *operation* that produces `context`: deterministic, per task, audited
   as a row, a new hash when edited. A ctags or tree-sitter symbol list
   ranked by the task's words is enough for one repo; no embeddings.

3. **Related work from our own store.** Nothing else in the survey has
   this: every landed task's summary, changed files, and hidden-test
   interface, plus every prior attempt in this lineage and why it
   ended. "The last attempt failed L1 test on planetesimals pricing; task
   33 landed light upgrades touching data.ts and tick.ts" is the
   200-token summary the literature found most effective, and it is a
   query, not a model call. This is a kernel operation, since it needs
   the store.

4. **Seeding whole files.** Paste the top files' contents. The agent
   reads them anyway, so tokens roughly break even under caching, but a
   wrong seed crowds out a right one and the budget is spent before the
   task starts. Prefer signatures (2) and paths, and let the agent read.

5. **Conventions the human writes.** A repo `CLAUDE.md` the CLI already
   loads: the namespace rule, "never pin exhaustive tables", how to run
   one test file, what is protected and why. The one kind of context the
   studies found reliably useful, and the one nobody has to generate.

6. **Learned file priors.** From our logs: the files successful attempts
   on similar tasks read first. A small, machine-derived hint that costs
   nothing to compute and is honest about where it came from.

## The design

One mechanism, three sources, one measurement.

- **Mechanism.** Operations may `produces = ["context"]`. What such an
  operation prints, cut to a per-step budget (default 1,500 tokens),
  is appended to the next directive's prompt under a heading, and
  recorded verbatim in the attempt's `inputs` so the audit shows exactly
  what the coder was told. A kernel operation `history` does the same
  from the store.
- **Sources, in the order to build them.** `history` first (3), because
  it is the strongest effect in the evidence and unique to us.
  `repo-map` second (2), ranked by task words, ctags-based. The repo's
  `CLAUDE.md` third (5), hand-written from the rules already in the
  preamble. The static map (1) rides along for free once (2) exists;
  seeding (4) not at all.
- **Measurement.** Two workflow versions differ only by the context
  operation; tasks alternate between them; the profiles compare turns
  before the first edit, tool calls, attempts, and cost per piece of
  work. `stats` gains "turns before first edit" as a column so the
  effect is read from data, not felt. If a source does not move the
  number, it goes.

## What not to build

An embeddings index, a context graph as infrastructure, an MCP gateway,
or a long generated overview. Right at Uber's scale, wrong at one repo
and one operator, and the studies say the overview would cost more than
it returns.
