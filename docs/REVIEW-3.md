# Third architectural review: the concurrent paths (2026-09-26)

The kernel's layers are now declared and enforced by a ratchet that can
only shrink. What the ratchet cannot see is concurrency: several
workers, and the CLI, the portal and the concierge beside them, all
writing one store. This review reads the concurrent paths cold, one
system at a time, and records a defect only once it has been
confirmed at its line, with the input that reaches that line and what
goes wrong there. Suspicions are not recorded. Each defect ends with a
paragraph a follow-up task can be filed from as written.

Sections: 1, the engine, the worker and the queue; 2, landing and
git; 3, the store, jobs and plugins. Each section is a separate cold
read, written by its own task.

## 1. Engine, worker and queue under four concurrent workers

Read as one system: src/engine.rs (1987 lines), src/worker.rs (1833),
src/queue.rs (1915), src/store/tasks.rs (1306), plus the lines they
call into when a question depended on them (src/agent.rs's stream
readers, src/ctx.rs `apply_grant`, src/environment.rs `recognize`,
src/git.rs `clone_task`, src/landing.rs's worktree re-creation,
src/concierge.rs's enqueue sites, src/store/attempts.rs
`latest_rate_limit`, src/view/projects.rs `maybe_settle_initiative`).
"Four concurrent workers" means `forge work --jobs 4` (four slots in
one process, on the multi-threaded tokio runtime from `#[tokio::main]`,
src/main.rs:69) and, equally, several worker processes (a successor
beside a draining worker, src/successor.rs) sharing one forge.db, with
the CLI, portal and concierge enqueueing from their own processes.

### 1.1 Defects confirmed while reading

1. **Enqueueing writes the whole row a second time after the task is
   already claimable, so a claim in between is undone and two workers
   can run one task.**
   `queue::enqueue` inserts the task as `queued` at queue.rs:641, then
   draws the journal arm and the explore arms (and loads
   `experiment.toml` from disk, queue.rs:656) and writes every column
   again with `update_task` at queue.rs:660, from its own in-memory
   `Task`, whose `state` is still `Queued`, `worker_pid` `None` and
   `started_at` `None`. `Store::update_task` (store/tasks.rs:534) is
   unconditional on state. The same shape is repeated with a wider
   window after `enqueue` has returned: `queue::answer` at
   queue.rs:1129 (an intake retry's plan) and the concierge at
   concierge.rs:145, 173 and 195. The last of these is the worst: it
   enqueues as `queued` and only then writes `Blocked` with a question,
   so a worker can start an intake run that was meant to wait for an
   answer.
   *Input:* any enqueue while a worker is polling: `forge add` from the
   CLI, the concierge in the portal's process, or `queue::file_plan`
   (engine.rs, the plan contract's `file_into_initiative`), which runs
   inside a worker slot while the main loop claims on another runtime
   thread; its first filed item has an empty `after` list and is
   claimable at once. *Reproduction:* (a) enqueue inserts task N;
   (b) worker slot B's `claim_next` sets N `running` with its pid
   (store/tasks.rs:669) and `run_task` reads the row, with no explore
   arms and no journal arm; (c) enqueue's `update_task` writes N back to
   `queued`, `worker_pid` NULL. Now either (d) another slot or process
   claims N while B is still cloning (B's own first write is
   engine.rs:214, after `prepare_worktree`, seconds later), and two
   runs of N clone into the same `worktrees/N` (see defect 2), or
   (d') B's write at engine.rs:214 puts N back to `running` with B's
   stale in-memory copy, erasing the arms enqueue drew: the task runs
   on unmeasured routing and the experiment's record of it is lost.
   *Task:* Make a newly enqueued task claimable only once its row is
   complete. In `queue::enqueue`, compute every column before the task
   becomes claimable: insert and write the journal and explore arms in
   one transaction, and make the post-insert write a narrow `UPDATE`
   of `journal`, `journal_arm` and `explore_json` guarded by
   `state='queued'`, never `update_task`. Give `queue::answer`
   (queue.rs:1129) and the concierge's three sites (concierge.rs:145,
   173, 195) a way to pass `plan`, `title`, `concierge_json`, and for
   the unclear case the `blocked` state and question, into the insert
   itself (fields on `TaskRequest`), so none of them rewrites a row
   after it was queued. Test: a store-level test that inserts through
   `enqueue`, claims the task between the insert and the arm write
   (a hook or by splitting the function), and asserts the task is
   still `running` with its arms recorded; and a concierge test that
   the "unclear" task is never observed `queued`.

2. **A worker that dies between cloning and its first task write
   leaves a clone directory that stops every later worker.**
   `prepare_worktree` clones into `worktrees/<id>` (engine.rs:558,
   `git::clone_task` at engine.rs:580) whenever `t.worktree` is empty
   (engine.rs:547), and sets `t.worktree` only in memory
   (engine.rs:614); the first store write of the run is
   engine.rs:214, after `prepare_worktree` returns, which for a retry
   from a verified branch includes a fetch, a reset and possibly a
   merge. *Input:* a crash, OOM kill or second SIGINT abort anywhere in
   that window. *Reproduction:* recovery requeues the task with its
   `worktree` column still empty (store/tasks.rs:919 changes only state,
   pid and reason); the next claim calls `clone_task` into the existing
   non-empty directory; `git clone` refuses ("destination path already
   exists and is not an empty directory"), and the result is classified
   `Env` at engine.rs:608. `worker::drive` requeues the task and the
   worker stops (`env_error`, worker.rs:1024-1029). The task is the
   oldest queued one, so the next worker claims it first and stops
   again: the whole queue is wedged until someone deletes the directory
   by hand. landing.rs:1043-1046 already removes a stale directory
   before re-creating a worktree; the engine does not.
   *Task:* In `engine::prepare_worktree`, when `t.worktree` is empty,
   remove a pre-existing `worktrees/<id>` before cloning (it can only
   be the remains of a run that never recorded it, since a recorded
   worktree takes the resume path), and persist `t.branch`,
   `t.base_sha`, `t.verify_base` and `t.worktree` with a store write
   immediately after the clone succeeds, before the verified-branch
   fetch and merge. Test: an e2e test that creates `worktrees/<id>`
   with a file in it for a queued task, runs `forge work --once`, and
   asserts the task runs (clone op ok) and the worker does not exit with
   "worker cannot run task".

3. **An environment grant with no sandbox is "fresh" every time, so a
   covered need loops forever.**
   `Forge::apply_grant` returns the grant for every call when there is
   no sandbox (ctx.rs:311, `(None, _) => true`), so `apply_environment`
   reports `Applied` on every call. In an operation step the rerun loop
   at engine.rs:696-702 has no bound; in a directive step
   engine.rs:1019-1022 refunds the attempt and goes round again,
   bounded only by the task's dollar cap. *Input:* an unsandboxed
   worker (no bubblewrap, or the sandbox disabled) and a failure whose
   text `environment::recognize` types as a need the `[environment]`
   policy covers. The defaults cover `~/.cache/ms-playwright`
   (environment.rs:48), and `missing_cache` matches any line with
   "doesn't exist" and a `/.cache/` path (environment.rs:196-215).
   *Reproduction:* a repository whose `test` check runs Playwright
   without browsers installed prints "Executable doesn't exist at
   /home/u/.cache/ms-playwright/chromium-1140/…"; the need is covered,
   `apply_grant` says fresh, nothing on disk changes, the check fails
   with the same line, and `run_operation_step` reruns it forever while
   holding a worker slot. An npm `403 https://registry.npmjs.org/…`
   does the same through `refused_host`'s URL branch.
   *Task:* Make each environment grant apply at most once per task
   whether or not there is a sandbox: in `Forge::apply_grant`, the
   unsandboxed arm returns `Some` only the first time a given grant is
   seen for that worktree (record applied grants per worktree the way
   the sandbox does, or consult `environment_grants` rows for the task
   through `crate::environment::record`'s table). Bound the rerun loop
   in `engine::run_operation_step` so the same need twice ends the
   loop (`Environment::Left`). Test: a unit test that `apply_grant`
   with `sandbox: None` returns `Some` then `None` for the same grant
   and worktree, and an e2e test with an unsandboxed fixture whose check
   always prints a covered cache-path line and fails, asserting the
   task fails after one rerun.

4. **A provider refusal recognized only from the result text refunds
   the attempt without holding the provider, and the directive
   relaunches at once, indefinitely.**
   The claude stream reader sets `rate_limited` from a `result` frame
   whose text mentions "rate limit" (agent.rs:640-643) but, unlike the
   `rate_limit_event` branch (agent.rs:675-682), records no window
   sample. The attempt row therefore has NULL `rl_*` columns, and
   `Store::latest_rate_limit` skips it (store/attempts.rs:424, "rl_five_hour
   IS NOT NULL OR rl_seven_day IS NOT NULL"), returning an older sample
   that is under the cap, stale (worker.rs:226) or absent.
   *Reproduction:* the refused run returns; engine.rs:1004-1012 refunds
   the attempt and `continue`s; the window check at engine.rs:892 finds
   no hold; the budget check passes (a refused run costs nothing, so
   `task_cost` does not move); a new agent is launched immediately with
   the same feedback. Nothing bounds the loop but the provider
   relenting. `provider_is_held` at claim time (worker.rs:272) reads
   the same missing sample, so the other slots claim tasks on the same
   provider and do the same.
   *Task:* A refusal that names no window must still hold the
   provider. In `agent.rs`'s claude `result` branch (agent.rs:640), when
   `rate_limited` is set and no `rate_limit_event` supplied a
   five-hour sample, set `out.rate_limits.five_hour = Some((1.0,
   now + 300))` as the `rejected` branch does; do the same in any other
   reader that sets `rate_limited` without a sample. In
   `engine::run_directive_step`, count consecutive refunds for
   refusals and stop with `End::Failed` (not counted) after a small
   bound, so a provider that keeps refusing cannot spin a slot. Test: a
   fake agent that returns an error result with "rate limit" in its
   text and no event, run twice; assert the second launch waits for the
   hold (the "rate … waiting" note) rather than starting at once.

5. **A resumed task trusts its old verifications after a rewind, so a
   killed re-attempt's commits can land without the code step or the
   review verifying them.**
   `run_task` rebuilds the cursor's `done` set from every `Succeeded`
   attempt ever recorded for the task (engine.rs:266-270), and
   `run_directive_step` skips a step in `done` that is owed nothing
   (engine.rs:812-820). A rewind (landing at engine.rs:1425, a
   verifying operation, the tests fault) clears `done` and records the
   owed feedback in memory only (`Run::rewind`, engine.rs:1655); the
   store keeps the old `Succeeded` attempts. *Input:* a workflow with
   a code step and a review (the default `reviewed`), a landing that
   fails and rewinds to the code step, and the worker dying (crash, or
   the second SIGINT, worker.rs:1053) while the re-attempt runs, after
   it has committed. *Reproduction:* `requeue` closes the killed
   attempt as `agent_failed`; the next worker's `done` holds both the
   code and the review seq from before the rewind, `owed` is empty, so
   both steps print "already verified; resuming"; the verifying
   operations run on the tree that now carries the killed attempt's
   half-finished commits, and `try_land` integrates it. If the checks
   pass, it lands with no agent having verified or reviewed those
   commits.
   *Task:* Rebuild the resume cursor from the record, not from every
   success ever recorded: a directive step counts as done on resume
   only if its latest agent attempt at that seq succeeded and no later
   attempt at an earlier directive seq exists (an attempt at an earlier
   step after it means the run was rewound past it). Put that rule in
   one function in engine.rs used where `Run.done` is built
   (engine.rs:266) and unit-test it on attempt lists: code ok, review
   ok, code agent_failed ⇒ neither is done; code ok, review ok ⇒ both
   done. Persisting owed feedback is optional; the rule alone stops the
   bypass.

6. **The attempt cap restarts at zero with every worker, and not only
   for attempts a dead worker ended.**
   `Run.used` starts empty for every run (engine.rs:264); its comment
   (engine.rs:1628-1629) justifies this as "a resumed task's earlier
   attempts were ended by a worker that died, not by the agent". That
   holds for the one attempt `requeue` closes, not for the attempts
   before it, which ended normally with `checks_failed` or
   `agent_failed`. *Input:* any requeue of a running task: the second
   SIGINT, orphan recovery, or an `Env` fault (`drive`, worker.rs:155),
   which includes the environment supervisor's own agent failing
   (engine.rs:443-445 classifies `env_supervisor::rule` as `Env`).
   *Reproduction:* a task with `max_attempts = 2` spends both code
   attempts, the second one's environment ruling fails as `Env`, the
   task is requeued and the worker stops; on the next start the code
   step gets two more attempts, and `AttemptStarted` reports "attempt 1
   of 2" for the third attempt. Only the dollar cap bounds the total.
   *Task:* Seed `Run.used` from the record: for each directive seq,
   count prior attempts at that seq except those closed by `requeue`
   (their reason starts "requeued"/equals the `requeue` reason, or
   mark them with a distinct state) and except refunded ones (rate
   limited, environment applied). Fix the comment at engine.rs:1628.
   Test: a unit test over a synthetic attempt list, and an e2e test
   that requeues a task after one failed attempt with `--retries 0`
   and asserts the next worker ends it failed without a second agent
   launch.

7. **Orphan recovery is keyed on pid liveness alone and runs only when
   a worker starts, so a restarted worker with its old pid never
   recovers its own tasks.**
   `recover_orphans` (worker.rs:849-857) runs once, at the top of
   `work` (worker.rs:876), and `Store::orphans` keeps a `running` task
   whenever its `worker_pid` is alive (store/tasks.rs:941), including
   when that pid is the recovering worker's own. *Input:* a worker
   restarted in a fresh pid namespace (a container whose entrypoint is
   `forge work`, which always gets the same low pid), or after a reboot
   where the old pid now belongs to another process. *Reproduction:*
   the container is killed mid-run; it restarts with the same pid;
   `orphans(pid_alive)` sees the task's pid alive (it is this process)
   and leaves the task `running` forever; nothing else ever requeues
   it, and its dependents never move. Separately, with two worker
   processes (a successor beside a draining worker), a worker that dies
   leaves its tasks `running` until some worker process starts again:
   the surviving worker never looks.
   *Task:* Record a worker identity that cannot be reused, not a bare
   pid: store the process start time alongside `worker_pid` (from
   `/proc/<pid>/stat` field 22 where available, else a random worker
   id written to `worker.pid`) and treat a `running` task as orphaned
   when its pid is dead, or its pid is alive with a different start
   time, or its pid is this process's own at startup. Run
   `recover_orphans` in the claim loop as well as at startup, so a
   surviving worker requeues a dead peer's tasks. Test: a store test
   that `orphans` returns a task whose `worker_pid` equals the caller's
   own pid when the caller says it just started, and a test that a
   stale start time is an orphan.

8. **A task fault ends the task without settling its initiative,
   releasing its dependents or emitting `TaskDone`.**
   `worker::drive`'s `Fault::Task` arm (worker.rs:138-154) writes
   `Failed` straight into the row and returns; everything `finish`
   does for a terminal state (engine.rs:1576-1614:
   `maybe_settle_initiative`, `release_dependents_of`, the `TaskDone`
   event) is skipped. *Input:* any `.task()` error in the run: a
   `forge.toml` that does not parse at the base (engine.rs:204), a
   recorded resolution that does not parse (engine.rs:519), a failed
   `git reset --hard` in the worktree. *Reproduction:* the last open
   task of an initiative fails this way; `maybe_settle_initiative` has
   exactly four callers (engine.rs:1577, landing.rs:1101, queue.rs:1177,
   supervisor.rs:876) and no backstop, so the initiative never settles
   and its `InitiativeSettled` notification never fires. Clients that
   re-read on events never see the task end. A dependent already
   `blocked` waiting on it is only re-evaluated by the claim loop's
   backstop (store/tasks.rs:835). The same loss happens when `finish`
   itself fails after its `update_task` (engine.rs:1575) and before the
   settle: the state is terminal, so `requeue` in `drive` does nothing,
   and the settle is never retried.
   *Task:* Give every terminal write one path. Move the tail of
   `engine::finish` (initiative settle, superseded settle, dependents'
   release, `TaskDone`) into a function both `finish` and `drive`'s
   `Fault::Task` arm call after writing the terminal state, and add
   `maybe_settle_initiative` for every initiative with no open tasks
   and no `settled_at` to the claim loop's backstop beside
   `release_dependents`. Test: an e2e test whose only initiative task
   has a base `forge.toml` that does not parse, asserting the task
   fails, a `task_done` event is emitted, and the initiative is settled.

9. **A resumed task is checked against the wrong provider at claim
   time, and a claimed task waits out a window inside its slot,
   deaf to shutdown.**
   `first_role` (worker.rs:250-268) returns the contract of the
   workflow's first directive, whatever the run has already done; its
   doc says it is "the role the task's *next agent step* will actually
   run under". `claim_next`'s `provider_held` (worker.rs:930) and
   `tightest_provider_hold` (worker.rs:284) decide on it. Once claimed,
   `run_directive_step` sleeps in a loop for as long as the step's own
   provider is held (engine.rs:892-901, up to an hour per turn of the
   loop, repeated until the reset), with no check of the worker's
   `stopping`. *Input:* a task requeued mid-run whose code step is done
   and whose next directive (the review) runs on a different, held
   provider; or a first directive on a free provider followed by a
   second on a held one. *Reproduction:* with `--jobs 4`, four such
   tasks are claimed (the claim check sees the free first provider),
   each reaches its held step and sleeps until the window resets, hours
   later; the worker's four slots are full of sleeping tasks while
   claimable work on free providers waits, and the first SIGINT
   ("running attempts will finish") waits for the reset too.
   *Task:* Judge a claim by the task's next directive, not its first:
   make `worker::first_role` skip directive steps the record already
   shows done (the same resume rule as defect 5), and when
   `run_directive_step` finds its provider held, end the run by
   requeueing the task (a new `End` or `StepFlow` variant that puts it
   back `queued` with the hold as its reason and frees the slot)
   instead of sleeping in it; the claim loop's existing hold logic then
   waits for the reset. Test: a unit test that `first_role` returns
   `review` for a task whose record shows the code step succeeded, and
   an e2e test where the review's provider is held and the task goes
   back to `queued` rather than occupying the worker.

10. **Dependents of a task that filed its plan into an initiative are
    blocked for good.**
    A plan step with `file_into_initiative` ends `End::Filed`, which
    maps to `Succeeded` (engine.rs:1724) with `landed_sha` empty, while
    `land` stays true from enqueue. `block_dependents` blocks any
    queued task waiting on a `succeeded`, `land = 1`, unlanded, finished
    task (store/tasks.rs:749), and `release_or_reblock` treats the same
    as a blocker (store/tasks.rs:356). *Input:* `forge add --after N`
    where N is a planning task that files its items.
    *Reproduction:* N files three tasks and succeeds; the dependent is
    blocked "waits on task N (succeeded: filed 3 task(s) into
    initiative 7)" and no later event releases it, although the work it
    waited for is now the filed tasks. The same happens for a
    dependency on a repository with no push remote, which ends
    `End::Verified` unlanded (engine.rs:1373-1381) although
    `dependency_fits` (queue.rs:254) accepted it.
    *Task:* Decide what a dependent of a filing task waits on and make
    the store say it: when `End::Filed` is recorded, re-point the
    task's dependents at the last filed task (`reroute_dependents(id,
    last)`, the retry mechanism already does this) so they follow the
    filed work. In `queue::dependency_fits`, refuse `--after` on a task
    whose repository has no push remote, naming why, as it refuses a
    `--no-land` task. Test: an e2e test with a filing plan task and a
    dependent, asserting the dependent waits on the last filed task and
    is released when that lands.

11. **The trust level's `per_day` cap is check-then-insert, so
    concurrent filers exceed it.**
    `enqueue` counts the level's tasks filed in the last 24 hours at
    queue.rs:591 and inserts at queue.rs:641, in separate statements
    with a configuration load, a workflow resolution and project
    lookups (and awaits) between them. *Input:* the public or contact
    trust level, filed from the portal and the concierge's plugin
    process at once. *Reproduction:* with `per_day = 5` and four filed,
    two concurrent requests both read 4, both pass
    `apply_trust_policy`, and both insert: six filed. The cap exists
    precisely for untrusted filers, who control the concurrency.
    *Task:* Enforce `per_day` in the insert: add a store method that
    inserts a task only if `COUNT(*)` of that trust level since the
    cutoff is below the cap, in one `BEGIN IMMEDIATE` transaction, and
    have `enqueue` call it when the level names a cap, returning the
    same refusal text. Test: a store test that runs two such inserts
    from two connections at the cap minus one and asserts exactly one
    succeeds.

### 1.2 Read and found sound

So the next reader does not repeat it:

- **Claiming is exclusive.** `Store::claim` is one conditional
  `UPDATE … WHERE state='queued'` (store/tasks.rs:669-675), checked by
  row count; `claim_next` tries candidates oldest first and returns only
  a row it won. WAL mode and `busy_timeout=5000`
  (store/mod.rs:350) make it hold across processes. Two claims of one
  row cannot both succeed; the only double run found is defect 1, which
  reverts the row, not the claim.
- **Crash between claim and first write**, apart from the clone
  directory (defect 2): the task is `running` with a pid and no other
  change, and orphan recovery requeues it with nothing lost.
- **`requeue`** (store/tasks.rs:919-930) runs two statements without a
  transaction; a crash between them leaves attempts closed and the task
  `running`, which the next recovery requeues again. Idempotent.
- **Conditional state writes** in `withdraw`, `set_task_fields`,
  `block_dependents` and `release_or_reblock` all guard on the expected
  state, so a worker claiming in between is left alone; `edit_task` and
  `queue::withdraw` refuse a running task.
- **Dependents' release** has a backstop on every claim loop
  (`release_dependents`, store/tasks.rs:835) as well as the trigger in
  `finish`; a crash between `finish`'s write and its release is healed
  there.
- **The run cursor and `StepFlow`** within one worker's run: `Next`,
  `Again` and `End` are handled at one match (engine.rs:312-319); every
  rewind goes through `Run::rewind`; each rewind is bounded by the
  directive's `used` count, and the landing rewind also by cost
  (engine.rs:1411). `End` alone decides push, state and reason
  (engine.rs:1712-1761), with unit tests over every variant.
- **Refunds** within a run: the rate-limit, environment and tests-fault
  refunds each undo exactly the increment made at engine.rs:920;
  `refund`'s `or_insert(1)` cannot go negative. `attempt_no` is never
  refunded, which is right: it numbers attempt rows.
- **Operation resume:** a mutating operation is skipped on resume only
  when the kernel's verify row at its seq is ok, and a verifying
  operation always runs again (engine.rs:686-693).
- **The early-ending watch** (agent.rs:602-623, 694-697) kills the child
  (`kill_on_drop` besides), records the signals, and the engine resumes
  the same session with `early_feedback`; each such attempt counts, so
  it is bounded by `max_attempts`. A fresh `Watch` per launch.
- **Provider holds across workers** read the shared store
  (`latest_rate_limit`, keyed by provider), so every slot and process
  sees one sample; a sample older than its window never holds
  (worker.rs:226). Defects 4 and 9 are about what writes the sample and
  which provider is asked, not about sharing it.
- **Shutdown:** signal streams are installed once for the worker's life
  (worker.rs:823-846); the second signal aborts the `JoinSet`, and every
  child process is `kill_on_drop` (agent.rs:544, checks.rs:167,
  git.rs:93), so no agent keeps writing into a requeued task's worktree.
- **An `Env` fault** requeues its own task and stops claiming; other
  slots finish. A panicking task stops the worker, whose restart
  recovers it.
- **Budgets checked at claim** (day budget, initiative holds) overshoot
  by at most the work already running; that is the documented design
  (a claim-time gate), not a race.
- **Store writes on the run path outside a transaction**, listed:
  engine.rs:214, 872, 1096, 1106, 1161, 1397 and 1575 (`update_task`
  from the run's own copy, which is safe because nothing else writes a
  running task's row except defect 1's enqueue sites); `op` rows and
  attempt rows (append-only); `requeue`, `block_dependents`,
  `reroute_dependents`, `release_*` (conditional or idempotent, above);
  `finish`'s tail (defect 8).

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
