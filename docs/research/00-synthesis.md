# Verification loop: what to build, in what order

Synthesis of the four reports in this directory, 2026-09-10. Each item names
the report it comes from and the Forge change it is. Where two reports
disagree, the disagreement is stated and resolved.

## What the reports agree on

- Tampering is rare on feasible tasks (1 to 3 percent) and common on
  impossible or ambiguous ones (20 to 80 percent), and frontier models do it
  mostly by editing tests directly (01). Guarding the tests closes most of
  it; an honest way out closes more than any monitor (01, 04).
- Deterministic checks are the verdict, but 8 to 30 percent of test-passing
  patches in the benchmarks are wrong or off-spec (04). That gap is the
  reason to add anything beyond checks, and the only additions with
  evidence are ones that execute code.
- Nothing should gate on a score. Mutation score (03) and judge confidence
  (04) are both explicitly rejected as gates; survivors and executed
  evidence are what is worth recording.
- One serial integrator per repo, rebase then re-check then fast-forward,
  is what every production merge queue reduces to at our scale (02).

## One disagreement, resolved

01 proposes an advisory diff-and-transcript judge as a flag. 04 finds that
diff-reading judges agree with tests at kappa 0.1 to 0.26 with 35 to 50
percent false-pass rates and concludes they are not worth building. 04 has
the measurements; 01 has the use case. Resolution: no diff-reading judge.
If a model verifier is built it is the executing, demote-only one in 04,
and its test-integrity section is where 01's rubric lives.

## Build order

### Before the first unattended night

1. **Protected paths, finished.** Done today as an L0 rule. Add the two
   cheap halves from 01: mount protected paths read-only in the sandbox
   under the same flag, so `sed` and `rm` cannot touch them either; and
   restore protected paths from the base commit before L1 runs, so even a
   task that was allowed to change them is graded by the operator's
   tests unless the operator said otherwise. (01 items 1 to 3.)
2. **The honest exit.** The prompt says plainly that stopping with
   `needs_input` when a check looks wrong or the task looks impossible is
   never penalized, and `needs_input` is recorded as a question, not a
   failure. The escalation channel cut hacking from 23.6 to 5.3 percent in
   the one study that measured it (01 item 5). Cost: one sentence and one
   state label.
3. **Hidden acceptance checks by default.** Today the `--check` commands
   are printed in the prompt. Hidden tests drive cheating to near zero at
   the cost of first-pass success when the task text under-specifies (01
   item 4). Default hidden, `--show-checks` to opt in when the checks are
   the spec.
4. **Baseline run.** Before an attempt starts, run the repo's checks on the
   base commit once per base SHA and cache the result: pass or fail, test
   counts, durations, output hash. Two things need it: the test-count row
   (03 and 01: tests executed and passed may not drop below the base run,
   which catches skips and deletions in test files that are not
   protected) and flake classification (item 6). It also gives the agent
   a check-ready worktree, since `setup` has already run.
5. **Budget in the right currency.** Not from the reports; from the
   account. Gate claiming on the five-hour rate-limit window, which is now
   recorded per attempt, with the dollar caps kept as a second guard.

### Days two and three

6. **Flake rule.** A failed check is rerun once on the same commit in a
   fresh sandbox with no agent turn between. Fail then pass is `FLAKY`,
   never `PASS`, and does not spend the agent's retry. Read against the
   baseline: flaky on base too means repo flakiness (proceed, stamp, ask
   the operator after three in ten); stable on base means the change
   introduced nondeterminism (do not push, one retry with both outputs).
   (03 recommendation b.)
7. **The integrator.** One serial queue per repo, FIFO by verification
   time. Rebase onto `origin/main` in a fresh sandbox worktree, re-run the
   full check set only if `main` moved, one retry, then push
   `<sha>:main --force-with-lease`. A conflict or a failure on the rebased
   result becomes a new attempt on the same branch with the conflict
   named, capped at one per move of `main` and two per task, then a human
   queue. Append-only journal per integration. No batching, bisection,
   priorities, or merge commits. (02.)

### Week two, with data

8. **`forge audit`.** Run the native mutation tool over the repo, store
   per-file caught, missed, timeout, and unviable counts plus the survivor
   list, print the score for humans, gate nothing. On Forge 2's own tree
   the sampled score was 54 percent in about ten minutes. Per task, a
   diff-scoped run after checks pass, survivors handed to the agent once
   as test goals, never a failure. (03 recommendation a.)
9. **The executing verifier.** Only after item 7 exists and there is a
   `needs_review` state for humans. Runs after every deterministic check
   passes, in a clean sandbox checkout, given the task, the acceptance
   commands, and a diff with test and check changes pre-flagged, never the
   worker's transcript, preferably a different model family. Two outputs:
   confirm, or demote to `needs_review` backed by at least one executed
   observation. Structured record with claims tagged executed, read, or
   inferred, a mandatory reproduction attempt, and an optional failing
   test Forge re-runs itself. Removed if demotion precision is under 50
   percent over 30 demotions, if it demotes over 30 percent of passing
   attempts, or if a no-verifier control arm shows the same later-defect
   rate. (04.)

## What not to build

A diff-reading judge (04). Mutation score as a gate (03). Batching,
bisection, or speculative integration (02). Any verifier that can promote
(04). A quarantine that hides a flaky test without asking (03).
