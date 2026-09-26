# Third architectural review: the concurrent paths, read cold (2026-09-26)

The kernel's layers are declared and enforced by a ratchet that can only
shrink. This review reads the paths where two things happen at once
(engine/worker/queue, landing/git, store/jobs/plugins) cold, one reader
per slice with no memory of how the code got here. Every defect is
confirmed at its line and written as a task text that can be handed to
Forge as it stands. Each section also lists what was read and found
sound, so the next reader does not read it again.

## 2. Landing, the integrator and git under concurrent landings

Read: `src/landing.rs` (all of it), `src/git.rs` (the `Git` runner, the
kernel repository and its lock, `clone_task`, `kernel_tree`,
`adopt_tree`, `stage`, `verification_checkout`, `place_branch`, `merge`,
`graft`, every push, `fetch_branch`, `remote_branch_exists`),
`src/engine.rs` (`prepare_worktree`, `try_land`, `publish`, `finish`,
the resume path in `run_task`), `src/operation.rs` (the mutating-step
commit), `src/cli/tasks.rs` (`land`, `integrate`), `src/supervisor.rs`
(its accept-and-land), `src/deploy.rs` (`run`, `origin_truth`),
`src/assess.rs` (`run_on_landing`), and `src/worker.rs` (fault handling,
`recover_orphans`). Lines are as of `c2bd2eb`.

Scenarios were worked through by hand: two tasks landing on one base
within seconds, a base that moves during the merged tree's verification,
a push to a non-bare remote with `receive.denyCurrentBranch=updateInstead`,
a crash at each point between the first push and the task row being
written, and two `land_task` calls for one task. Where a finding depends
on how git behaves, the behaviour was checked against git 2.5x in a
scratch repository; those experiments are noted as *reproduced*.

### 2.1 Defects confirmed while reading

In order of cost. None needs a refactor.

1. **A failed `ls-remote` lands the operator's local base branch.**
   `remote_branch_exists` returns `false` on any failure, not only on
   "no such branch" (git.rs:1118-1124: `.map(|o| o.status.success() &&
   !o.stdout.is_empty()).unwrap_or(false)`). The landing reads that
   `false` as "the remote has no base yet" and takes the base from the
   registered checkout's local `refs/heads/<base>` (landing.rs:270,
   295-299). *Reproduction:* the operator's checkout has two local,
   unpushed commits on `main`; a task finishes; the `ls-remote` at 270
   fails (a network blip, an ssh agent timeout, a rate limit, *reproduced*:
   exit 128 on an unreachable URL). `main_sha` is now the local tip, which
   descends from the remote's. It is merged into the branch, verified,
   and pushed as the base at 537. The remote accepts it because it is a
   fast-forward, so the operator's unpublished commits are published
   under the task's name. If the local base is instead *behind* the
   remote, the push is rejected and the round repeats. The same
   conflation at engine.rs:552 and 562 picks a branch name that already
   exists, or starts a task from the local base, and says nothing.
   *Task:* "`git::remote_branch_exists` conflates 'no such branch' with
   'could not ask': make it return `Result<bool>` (an `ls-remote` that
   exits non-zero is an error; success with empty output is `false`). In
   `landing::integrate` a failed probe is the worker's environment, the
   same as a failed fetch (`Fault::Env`, with an `integrate` op row); only
   a successful empty answer may take the base from the registered
   checkout. Update the callers in engine.rs (branch naming, base ref)
   and landing.rs (`integrate_many`, `land_task`). Add an e2e test in which
   the remote URL is unreachable at landing and the registered checkout's
   `main` is ahead of the remote: the task must not land, and the remote's
   `main` must be unchanged."

2. **Under `cheap`, a formatter commit makes the landing fail.**
   `last_verified_sha` returns the `end_sha` of the last successful
   *attempt* (landing.rs:94-103). A mutating operation commits in the
   task's clone after the directive (operation.rs:317-333) and records
   its kernel verify as an *op* row, not an attempt. So when `fmt`
   changes anything after `fix` (src/builtins/workflows/cheap.toml:3-8),
   the clone's HEAD is the `forge: fmt` commit, `staged != verified`, and
   the landing ends `Failed("the branch moved before landing began")`
   (landing.rs:230-256). The kernel itself verified that commit. Every
   `cheap` task whose formatter touches a file fails at landing, and a
   `Failed` task cannot be landed by hand (landing.rs:1009). No test
   lands a workflow whose last step is a mutating operation that commits.
   *Task:* "The landing guard in `landing::integrate` compares the
   branch with the last successful attempt's `end_sha`, but a mutating
   operation (`fmt`) commits after the directive and is verified by a
   kernel op row, not an attempt. Record the commit a mutating operation's
   kernel verify judged (for example as the op row's detail sha or a new
   `end_sha` on ops) and have `last_verified_sha` take the later of the
   two. Add an e2e test: the `cheap` workflow with a fake `fix` agent that
   leaves a file unformatted and a `fmt` that changes it; the task must
   land."

3. **The landing is not on the record until deploy and assess finish,
   and both run under the repository lock.** `integrate` calls
   `deploy_on_landing` and `assess::run_on_landing` (landing.rs:639-640)
   before it returns `Landed`. `_lock` (221) is held until that return.
   The task row learns `landed_sha` only afterwards: in memory at
   engine.rs:1386-1389 and on disk at `finish`'s `update_task`
   (engine.rs:1575), or at landing.rs:1089-1098 for `forge land`. A deploy
   runs the target's check, its smoke check and the deploy look (an
   agent), and assess is a second agent. Two costs follow.
   (a) *A crash window of minutes.* A worker killed, or a `forge land`
   interrupted with Ctrl-C, after the base push at 537 has moved the base
   while the task is still `running`, `succeeded` or `blocked` with no
   `landed_sha`. For a worker, `recover_orphans` requeues it
   (worker.rs:849-852). The resumed run skips its verified directives,
   lands again (a no-op merge or fast-forward, then a full
   re-verification), and pushes nothing new ("Everything up-to-date"
   exits 0, *reproduced*). It then **deploys and assesses a second time**
   and writes a second `land` row. For `forge land` nothing resumes: the
   task stays unlanded while its commit is on the base, and dependents
   are not released until someone lands it again, which redeploys.
   (b) *Serialized behind agents.* A second task's landing on the same
   repository waits on the flock (landing.rs:147) through the first
   task's deploy, deploy look and assessment.
   *Task:* "Split the landing's side effects from the landing. Have
   `landing::integrate` return `Landed(sha)` right after the base push
   and the forge-verify fold, still under the repository lock. Have the
   callers (`engine::try_land`, `landing::land_integrated`) persist
   `landed_sha`, `landed_at` and the reason at once with `update_task`,
   and only then, with the lock released, run `deploy_on_landing` and
   `assess::run_on_landing`. A resumed or repeated landing whose base
   already contains the task's verified commit must record it as landed
   without re-running deploy or assess (check `landed_sha`, or that the
   remote base is the candidate, before the side effects). Add an e2e test
   in which a deploy check sleeps and a second task on the same repository
   lands while it runs."

4. **A rejected base push is always read as "the base moved".** On any
   error from the base push, landing.rs:537-566 emits "moved underneath;
   integrating again" and loops. When the base did not move, round 1
   finds `main_sha == base_sha` (301), merges nothing, **re-runs the whole
   merged-tree verification** (374-388), and pushes again. Round 2 does
   the same, and then the task is `Failed` with `pushes: false`
   (engine.rs:1442-1446): two wasted full check runs and a false note. The
   case that matters is a non-bare remote with
   `receive.denyCurrentBranch=updateInstead`, which is the only way to push
   straight into a checked-out branch:
   - A dirty remote working tree makes receive-pack refuse the push
     *before* moving the ref ("Working directory has unstaged changes",
     *reproduced*). The retry loop cannot fix that, and the task fails
     for its environment.
   - A push that updates the remote's working tree but not its ref is
     possible, and Forge never names it. `updateInstead` runs `read-tree
     -u -m` on the remote tree first and takes the ref lock second. If
     the ref update fails (the remote user's own `git` holds
     `refs/heads/main.lock`), the push is rejected, the remote tree and
     index now hold the landed tree, and HEAD is unmoved (*reproduced*:
     "cannot lock ref", then `git status` on the remote shows the change
     staged). From then on, every push to that branch is refused with
     "Working directory has staged changes" (*reproduced*). Forge spends
     rounds 1 and 2 re-verifying, fails the task with that message, and
     every later landing on the repository fails the same way until a
     human resets the remote checkout. The remote now shows the task's
     change staged, as if a person had made it. The reverse, a push that
     moves the ref and leaves the tree stale, cannot come from
     receive-pack, which updates the tree first. It can only look that
     way from the client when the connection drops after the ref moved.
     That case is sound (see 2.2).

   *Task:* "In `landing::integrate`, tell a non-fast-forward rejection of
   the base push apart from every other rejection. Parse `git push
   --porcelain` output in `git::push_sha`, or re-read the remote base with
   `ls-remote` after a failure: only a base that actually moved goes round
   again. A rejection with the base unmoved ends the landing at once as
   the environment's problem (`Fault::Env`, or a blocked task with a
   question naming the remote's message, e.g. 'Working directory has
   staged changes'), with no further verification. Add an e2e test with a
   non-bare remote using `receive.denyCurrentBranch=updateInstead` and a
   dirty working tree: the landing must run the merged-tree checks once,
   and the task must not end as a failed attempt at the code."

5. **A later landing round can strand the branch pushed in an earlier
   round.** Round 0 pushes the merged candidate as the task branch
   (landing.rs:492) before the base push. If the base push is rejected
   and round 1's merge of the newer base conflicts, the conflict path
   (322-363) places the new base in the clone but does not adopt the
   landing tree, as the verify-failure path does at 391-393. It also
   returns as `base_sha` round 0's base (set at 367-368), which the clone
   does not contain. The coder then resolves on top of its *original*
   HEAD. The next landing pushes a commit that does not descend from the
   round-0 candidate on the remote. `push_sha` is unforced (git.rs:970),
   so the branch push is rejected as non-fast-forward (*reproduced*), and
   the task ends `Failed("push of … failed")` after the coder did exactly
   what it was told. A crash between the branch push (492) and the base
   push (537) ends the same way when the base had to be merged. The
   resumed run re-merges into a fresh kernel tree, the merge commit gets
   a new timestamp and so a new id, and the branch push is rejected.
   *Task:* "`landing::integrate` pushes the task branch in round 0 and
   can then rewind from a later round, or resume after a crash, with a
   branch that does not descend from what it pushed. Push the task branch
   only once the base push has succeeded (it is a record, not an input to
   the base push). On a conflict in any round after the first, adopt the
   landing tree into the clone before placing the new base, as the
   verify-failure path does, so the returned `base_sha` is one the clone
   contains. Add an e2e test in which the remote base moves during the
   merged-tree verification, twice, the second time with a conflict; the
   coder resolves it and the task lands."

6. **Assess fails on every landing that had to merge the base, which is
   exactly the concurrent case.** `assess::try_run` runs `git diff
   <t.base_sha> <landed_sha>` in `t.worktree`, the agent's clone
   (assess.rs:120). When the landing merged a moved base, the landed
   commit is a merge created in the kernel's landing tree (312). It
   exists only there, in the kernel repository and on the remote, never
   in the clone, so the diff fails with "bad object" and the landing
   notes "assess failed". Even with the object present, `t.base_sha` is
   the task's *starting* base (landing never updates it on `Landed`,
   landing.rs:201-203), so the diff would include every change other
   tasks landed in the meantime, and the assessor would score their work
   as this task's. The e2e assess tests (tests/e2e/landing.rs:1421, 1467)
   only land on an unmoved base.
   *Task:* "`assess::run_on_landing` diffs `t.base_sha..landed_sha` in the
   agent's clone. After a landing that merged the base, the clone lacks
   the merge commit, and `t.base_sha` predates other tasks' landings. Pass
   the assessment the base the landing actually verified against (the
   first parent of the landed merge, or the `base_sha` `integrate`
   settled on) and run the diff in the kernel repository, which has both
   commits. Add an e2e test: two `reviewed` tasks on one repository, the
   second landing after the first so that its landing merges; the second
   task's assessment must be stored and its prompt's diff must name only
   the second task's file."

7. **Two `land_task` calls for one task both land it.** `land_task`
   checks `landed_sha` (landing.rs:1015), recreates a collected worktree
   into the fixed path `worktrees/<id>` (1043-1050), and counts `seq` and
   `attempt_no` (1081-1082), all **before** `integrate` takes the lock
   (221). The supervisor's accept-and-land (supervisor.rs:843) and the
   operator's `forge land` or inbox button can run together. The second
   call waits on the lock, then lands a task that is already landed. Its
   merge is a no-op, it re-verifies, its pushes do nothing, and it
   deploys and assesses again, emits a second `TaskDone`, duplicates op
   `seq` numbers, and overwrites `hand_landed` with its own `by_hand`
   (1092). That value feeds the human-attention statistics. With a
   collected worktree, the two calls also delete and re-clone the same
   directory under each other.
   *Task:* "Make `landing::land_task` take the repository lock
   (`repo_lock`) before it reads the task, and hold it through the
   worktree recreation and `integrate` (have `integrate` accept a held
   lock rather than take its own). Re-read the task under the lock and
   refuse one that already has a `landed_sha`. Add a unit or e2e test that
   runs two `land_task` calls for one task concurrently: exactly one lands,
   the other errors with 'already landed', and one `land` op row is
   written."

8. **The landing reads the base through the operator's checkout.**
   `fetch_branch` runs `git fetch <remote> <base>` in the registered
   checkout and then reads `refs/remotes/<remote>/<base>` (git.rs:432-437).
   A fetch with no destination updates that ref only *opportunistically*:
   only if the remote's configured fetch refspec covers the branch. A
   checkout cloned `--single-branch` of another branch, or with a narrowed
   `remote.origin.fetch`, keeps a stale tracking ref, while `FETCH_HEAD`
   has the new tip (*reproduced*). The landing then merges and verifies a
   stale base, the base push is rejected, and the stale read repeats for
   three rounds before `Failed`. Separately, a failed fetch is
   `Fault::Env` (landing.rs:292), and an environment fault from a task
   stops the whole worker (worker.rs:154-158, 1025-1029). The same
   checkout is fetched without the landing lock by every task start
   (engine.rs:563) and by `forge integrate` (landing.rs:851), and the
   operator's own git runs in it too. So a transient failure there, such
   as a held `refs/remotes/origin/main.lock` or a flaky network, takes
   the worker down, not just the one landing.
   *Task:* "Fetch the base for landing into the kernel repository instead
   of the registered checkout. Use `git::stage(home, repo, url,
   refs/heads/<base>, refs/forge/origin/<base>)`, the pattern
   `deploy::origin_truth` already uses: it fetches by explicit refspec
   under the kernel lock and returns the sha. Retry a failed fetch once
   before treating it as the environment. Keep the registered checkout's
   tracking ref updated best-effort only, for the operator. Add an e2e
   test with a registered checkout whose `remote.origin.fetch` does not
   cover the base branch and a remote base that moved: the task must land
   on the remote's tip in one round."

9. **Deploy-on-landing drops its errors, and can leave a deploy row
   open.** `deploy_on_landing` discards `deploy::run`'s result
   (landing.rs:662, `let _ =`). Its comment says `deploy::run` "already
   emits its events", which is true only after the `DeployStarted` event.
   Every `?` before it (target lookup, `config::load_working`,
   `rev_parse` of the sha) fails with no event and no row. After it,
   `start_deploy` inserts a row (deploy.rs:223), and a failing
   `deploy_at(...).await?` (225-234) returns without `finish_deploy`,
   leaving that row open for good. On a merged landing, a non-self
   target archives the landed sha from the registered checkout
   (deploy.rs:202-209 and `fresh_archive`). The sha only gets there
   through the best-effort fetch at landing.rs:569 (`let _ =`), so when
   that fetch fails, the deploy fails as well, silently.
   *Task:* "`landing::deploy_on_landing` must not discard
   `deploy::run`'s error: emit it as a `deploy` note on the task. In
   `deploy::run`, finish the deploy row (check not ok, reason = the
   error) on every error after `start_deploy`, not only on a failed
   check. Archive a non-self target's landed sha from the kernel
   repository, which always has it after a landing, rather than from the
   registered checkout. Add an e2e test with an on-landing target whose
   method errors before running: the task's trace names the error and no
   deploy row is left unfinished."

10. **The forge-verify fold never catches up with the remote.** The fold
    grafts onto the registered checkout's local `refs/heads/forge-verify`
    (landing.rs:586-595, git.rs:582-653) and pushes it unforced
    (597-615). Nothing ever fetches `forge-verify` from the remote. If
    the remote's `forge-verify` moved (the audit tells the operator to
    "change the test on the forge-verify branch", audit.rs:183, and a fix
    made in any other clone is pushed to the remote), every later fold is
    rejected. The only report is a suffix on the `land` row's detail
    ("folded into forge-verify locally (push failed: …)"). Verification
    also keeps overlaying the stale local suite (`overlay_refs`, 684-686),
    so the operator's fix to a contradicting test never reaches a task.
    *Task:* "Before folding a task's hidden tests, fetch the remote's
    `forge-verify` and graft onto whichever of local and remote is ahead.
    If they diverged, stop the fold with a note naming both shas rather
    than pushing blindly. Emit a failed fold push as its own `Note`, not
    only inside the `land` detail. Add an e2e test in which the remote's
    `forge-verify` gains a commit from another clone before a landing:
    the fold must include it, and the next verification must overlay
    it."

### 2.2 Read and found sound

- **Two landings on one base within seconds.** `repo_lock`
  (landing.rs:134-151) is a `flock` on `FORGE_HOME/locks/<path>.lock`,
  taken for the whole of `integrate`. Repository paths are canonicalized
  at registration (cli/tasks.rs:192, cli/projects.rs:324), so one
  repository has one lock, and the lock holds across processes: the
  worker, a successor worker draining alongside it, the supervisor
  inside the worker, and `forge land` in the CLI. The second task's
  round 0 fetches the first task's tip, stages it into the kernel,
  places it and merges it into its own branch. It reloads the
  configuration *from the new base* (370), overlays the current
  `forge-verify` tip (372), which by then includes the first task's
  folded tests, and verifies before pushing. The only cross-talk is 2.1
  item 6. The sanitizing map in `repo_lock` can give two distinct paths
  one lock file (`/a/b-c`, `/a/b_c`); that over-serializes but is never
  unsafe.
- **A base moved by someone outside Forge during verification.** The base
  push is by object id from the kernel repository, never forced
  (git.rs:962-974), so the remote enforces fast-forward atomically on
  its side. Round 1 merges the newer base into the *landing tree's* HEAD,
  which already holds round 0's merge, and verifies again against the
  reloaded configuration. Three rounds, then `Failed`, is a deliberate
  bound. The two sha guards bind what is pushed to what was verified:
  the branch must equal the last verified commit before landing starts
  (230-256), and the merged tree must not have moved during verification
  (465-491).
- **A push that moved the remote ref but reported an error** (a
  connection dropped after receive-pack's update). Round 1 fetches the
  base, which is now the candidate itself. `is_ancestor` holds, so there
  is no merge, `base_sha` becomes the candidate, and one extra
  verification runs. Then both pushes are no-ops that exit 0
  (*reproduced*), and the task lands. The only cost is that
  verification.
- **A crash before the branch push.** Nothing has left the machine. The
  task is requeued as `running` (worker.rs:849-852, store/tasks.rs:919-944),
  its verified directives are skipped on resume (engine.rs:266-270), and
  the landing reruns from the top. A crash after the base push is 2.1
  item 3, and one between the two pushes is 2.1 item 5.
- **The kernel repository's locks.** `kernel_lock`
  (`FORGE_HOME/repository-locks/<sha256>`, git.rs:230-239) guards every
  ref *write* into the kernel repository: `stage` (300),
  `verification_checkout` (328) and `place_branch`'s `update-ref` (460).
  It is never taken while already held, and it is only ever taken after
  `repo_lock`, never before, so the two cannot deadlock. The kernel
  repository is first created only inside `stage` or
  `verification_checkout`, both under the lock. The unlocked uses
  (`kernel_tree`'s clone, `push_sha`, `published`'s `ls-remote`) all
  come after a staged ref names what they read. The shared
  `refs/forge/placed/forge/<base>` is written and read only by the
  landing, under `repo_lock`. The per-task refs
  (`refs/heads/<branch>`, `refs/forge/outgoing/<branch>`) are named by
  branch, and branch names are unique per task (engine.rs:549-556).
  *Not confirmed, noted for the next reader:* `KERNEL_CONFIG`
  (git.rs:214) does not set `gc.auto=0`, so a fetch into the kernel can
  start a detached auto-gc that outlives the lock. `kernel_tree` makes a
  local `--no-hardlinks` clone, which copies object files without the
  lock, so a repack that deletes a pack mid-copy could fail that clone.
  This was not reproduced.
- **Merge hygiene.** `git::merge` aborts a conflict and reports the paths
  (git.rs:489-520), so it never leaves a tree conflicted. `TempTree`
  removes the landing tree and its provider state on every return path
  (landing.rs:121-130), including after `adopt_tree` has already moved
  it.
- **The fold is idempotent.** `graft` returns `None` when the branch
  already has exactly those contents (git.rs:641-645), so a repeated
  landing, as in 2.1 items 3 and 7, folds nothing twice.
- **`forge integrate` (`integrate_many`) never touches the base.** It
  merges into a scratch clone, verifies with every hidden suite after
  each task, and leaves a branch in the registered checkout for a human
  to fast-forward (landing.rs:821-971). Its fetch at 851 shares the
  exposure described in 2.1 item 8. Its `integrate-<unix second>`
  directory and branch would collide only for two runs in the same
  second, which is an error rather than a wrong result.
