# 02. Merge queues: verifying on the merge result

## The question

Production merge queues all solve the same problem Forge has: two branches that each pass checks can still break `main` once both land, so the check has to run on the merge result, not the branch. This report surveys how the major queues verify, batch, bisect, order, and handle conflicts, then picks the subset that fits one machine, 5 to 30 second checks, and one to four agents.

## Designs in the field

| System | What is verified | Batching and failure handling | Conflict | Always-green guarantee | Cost model |
|---|---|---|---|---|---|
| bors-ng | Merge commit of base plus PRs on a `staging` branch; on pass, base is fast-forwarded to it | Yes. Failing batch is split in two and requeued; a batch of one that fails is "kicked back to the creator" | PR dropped and reported | Yes: base only moves to a tested commit | One CI run per batch; claimed O(E log N) |
| rust-lang/bors | Merge of PR with latest base on `automation/bors/auto`; on pass, base is fast-forwarded | None automatic: "Only one auto build runs at a time." Humans build rollup PRs and bisect failed rollups by hand | Unmergeable PR fails at the merge step and waits for its author | Yes | Serial; 3.5 hour CI is why rollups exist |
| Zuul | Each change tested "exactly as it is going to be merged": A, A+B, A+B+C in parallel | Speculative window (default 20, floor 3, halves on failure, grows by 1 on success). Failing change removed, everything behind it re-tested | Unmergeable change dequeued | Yes | Parallel CI; runs wasted when an early change fails |
| GitHub merge queue | Temporary branch of base plus every queued PR ahead, as a `merge_group` event | Optional groups (min/max entries, wait timeout). Failed PR removed, later groups rebuilt; no bisection inside a group | Conflicting PR removed from queue | Yes, if required checks run on `merge_group` | Concurrency-throttled parallel builds; queue jump rebuilds everything |
| GitLab merge trains | Merged-result pipeline per MR: A+target, A+B+target, ... in parallel | No batching. Failed MR removed, "new pipelines for all the merge requests that were queued after it" | MR dropped with a system note | Yes | Up to 20 parallel pipelines |
| Uber SubmitQueue | Speculative merges along a speculation tree; conflicts defined by shared build targets; a logistic model picks which combinations to build | Speculation is the batching. Failure re-plans and stops builds not in the new plan | Independent changes commit in parallel | Yes: "the illusion of a single queue" | Turnaround 1.2x an oracle |
| Chromium CQ | Patch on tip of tree; failing shards retried, then run "without patch" to filter pre-existing failures | No batching. Real failure: CL not submitted | Not covered in fetched doc | Best effort; sheriff and tree status backstop | Retries as the flakiness answer |
| Google TAP | Presubmit: fast tests against head. Postsubmit: a milestone about every 45 minutes runs each affected target once | Milestone is the batch; failing batch split into single changes; rollback is the primary fix | Not applicable | No hard guarantee: "true head" vs "green head" | Batching is what makes 4.2M-test milestones affordable |
| Graphite | Draft PR of every stack rebased onto the previous; on pass, `main` fast-forwarded to that HEAD | Yes. Failing batch bisected; passing stacks requeue | Not detailed in fetched docs | Yes at head; intermediate commits may not build | Parallel CI plus batching |
| Mergify | Temporary batch PRs of cumulative merges (PR1, PR1+PR2, ...) | Yes. `batch_size`, `batch_max_wait_time`; failed batch split into `max_parallel_checks` parts, bounded by `batch_max_failure_resolution_attempts` (0 dequeues the batch); a batch of one that fails is the culprit | Caught by pre-merge update; PR removed | Yes | Wasted CI traded for latency |

Two observations. Every system that promises a green target uses the same primitive: build the exact commit you intend to land on a scratch ref, test it, move the target with a fast-forward. Nobody re-tests after the move. And the two designs that do not batch (rust bors, Chromium) have the slowest CI and compensate with humans. Batching is a latency optimization for long checks; bisection is the tax on batching.

Unverified: the Uber PDF returned 403; its row comes from the ACM abstract in search results and Colyer's summary. Graphite's docs index returned 404; its row comes from the optimizations page and batching post.

## What matters at Forge's scale

One machine, checks in seconds to low minutes, at most four branches waiting, and an author that can be re-run. That changes the trade-offs.

Matters:

- Verify the exact commit that will become `main`, then fast-forward. The one universal rule, cheap here, and exactly the Forge 1 integrator.
- Serial integration. With one worker, speculative parallelism (Zuul, GitLab, SubmitQueue) buys nothing, and a serial queue has no wasted runs and no re-planning logic.
- Conflict routing. Same-night branches touch the same files, so conflict is the common case. Every system above dequeues and waits for a human; Forge can re-run the author.
- One bounded retry for spurious failures (Chromium's retry pattern, rust's `@bors retry`), journaled.
- A journal with before and after SHAs, so any move of `main` is explainable and revertable (TAP: rollback is the fastest fix).

Does not matter:

- Batching and bisection. Batching amortizes hours-long CI; with 30 second checks, four branches serially cost two minutes, and bisection never needs to exist.
- Speculative windows, speculation graphs, conflict prediction, queue jumping, priorities, path-scoped queues, culprit finding. The culprit is always the one branch being integrated.

## Recommendation for Forge

One integrator, serial, verify-then-fast-forward. Nothing else.

**State machine.** Entry is `verified` (checks passed on the branch in the sandbox, pushed as `forge/<id>-<slug>`). Then:

1. `queued`: the branch is in the integrator's FIFO, ordered by verification time. One queue per repo.
2. `integrating`: the integrator takes the repo lock, fetches `origin/main`, and runs `git rebase origin/main` on the branch in a fresh sandbox worktree. Rebase, not merge commit, so `main` stays linear, as in Forge 1.
   - Rebase applies cleanly: continue to step 3.
   - Rebase conflicts: go to `conflicted` (see below).
3. `checking`: run the full check set (typecheck, lint, tests, build, acceptance commands) on the rebased result, exactly as verification does. If `origin/main` has not moved since the branch was verified, skip: the branch commit already is the merge result.
   - Pass: step 4.
   - Fail: retry once (the branch passed these checks minutes ago, so a second failure is a real interaction with `main`, not flakiness). Second failure: `rejected-on-main`.
4. `landing`: force-update the task branch to the rebased commit (the only non-fast-forward push, never to `main`), then `git push origin <sha>:main --force-with-lease=main:<before-sha>` so the push succeeds only if `main` is still where the check ran. Lease failure means someone else pushed: back to step 2. Never force-push `main`.
5. `merged`: terminal. Journal written, then the remote task branch deleted.

**Conflict handling: hand it back to the agent as a new attempt.** On `conflicted`:

- Record the conflict (files, both SHAs) in the journal and the task.
- Open a new attempt on the same task: "rebase `forge/<id>-<slug>` onto `origin/main` at `<sha>`, resolve conflicts in these files, keep the task's intent, re-run checks." The agent starts from the existing branch. Its output goes through normal verification and re-enters the queue.
- Cap at one rebase attempt per move of `main` and two per task. Past the cap, or if the attempt fails verification, the task goes to the human conflict queue with the journal entry attached. Forge 1 stopped at the human queue; the agent attempt is the one addition.
- `rejected-on-main` gets the same treatment: one agent attempt carrying the failing check output, then the human queue. This is what batching systems call "culprit found"; here it is free.

**What runs where.** The integrator is one daemon task holding a per-repo lock; rebase and checks run in the verifier's sandbox in a fresh worktree. Other agents keep working on their own branches meanwhile and pay the rebase later.

**Ordering.** FIFO by verification time, no priorities. If two verified branches conflict, the first lands and the second gets the rebase attempt; no policy needed.

**Journal.** One append-only record per integration: task id, branch, branch SHA at queue time, `main` SHA before, rebased SHA, check results with durations, outcome (`merged`, `conflicted`, `rejected-on-main`, `lease-failed`, `human-queue`), `main` SHA after, and the id of any attempt spawned. It should read as a linear history of why `main` moved.

**Deliberately not built.** No batching, bisection, speculative parallel integration, priorities, merge commits, queue jump, path-scoped queues, flakiness model beyond one retry, or rollback automation (the journal makes `git revert` a one-liner). Revisit batching only when journaled wait time shows checks over roughly ten minutes and a nightly queue of more than a handful of branches.

## Sources

- bors-ng documentation: https://bors.tech/documentation/
- bors-ng README (staging branch, bisecting, fast-forward): https://github.com/bors-ng/bors-ng
- rust-lang/bors README: https://github.com/rust-lang/bors
- rust-lang/bors design document: https://raw.githubusercontent.com/rust-lang/bors/main/docs/design.md
- Rust Forge, rollup procedure: https://forge.rust-lang.org/release/rollups.html
- Zuul, project gating: https://zuul-ci.org/docs/zuul/latest/gating.html
- Zuul, pipeline configuration (dependent manager, window settings): https://zuul-ci.org/docs/zuul/latest/config/pipeline.html
- GitHub, managing a merge queue: https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue
- GitHub, merging a pull request with a merge queue: https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/incorporating-changes-from-a-pull-request/merging-a-pull-request-with-a-merge-queue
- GitLab, merge trains: https://docs.gitlab.com/ci/pipelines/merge_trains/
- Uber, Keeping Master Green at Scale (EuroSys 2019), ACM record (PDF not fetched, 403): https://dl.acm.org/doi/10.1145/3302424.3303970
- The Morning Paper summary of the Uber paper (secondary): https://blog.acolyer.org/2019/04/18/keeping-master-green-at-scale/
- Chromium, Commit Queue: https://chromium.googlesource.com/chromium/src/+/main/docs/infra/cq.md
- Software Engineering at Google, chapter 23, Continuous Integration (TAP): https://abseil.io/resources/swe-book/html/ch23.html
- Memon et al., Taming Google-Scale Continuous Testing (TAP milestones): https://static.googleusercontent.com/media/research.google.com/en//pubs/archive/45861.pdf
- Graphite, merge queue optimizations: https://graphite.com/docs/merge-queue-optimizations
- Graphite, cheaper CI and faster merging with batching: https://graphite.com/blog/merge-queue-batching
- Mergify, merge queue overview: https://docs.mergify.com/merge-queue/
- Mergify, batches: https://docs.mergify.com/merge-queue/batches/
- Mergify, parallel checks: https://docs.mergify.com/merge-queue/parallel-checks/
