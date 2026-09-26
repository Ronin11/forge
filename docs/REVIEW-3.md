# Third architectural review: the concurrent paths (2026-09-26)

The kernel's layers are now declared and enforced by a ratchet that can
only shrink. What the ratchet cannot see is concurrency: several
workers, and the CLI, the portal and the concierge beside them, all
writing one store. This review reads the concurrent paths cold, one
system at a time, and records a defect only once it has been
confirmed at its line, with the input that reaches that line and what
goes wrong there. Suspicions are not recorded. Each defect ends with a
paragraph a follow-up task can be filed from as written.

Sections: 1, the engine, the worker and the queue (this read); 2,
landing and git; 3, the store, jobs and plugins. Sections 2 and 3 are
separate reads and are written by their own tasks.

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
