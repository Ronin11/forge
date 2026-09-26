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

## 3. The store under concurrent writers, job recovery, plugin supervision

Read: `src/store/` whole (the connection, `migrate`, every write in
`tasks.rs`, `jobs.rs`, `workers.rs`, `attempts.rs`, `deploys.rs`,
`events.rs`, `record.rs`), the callers that pair store writes
(`queue::enqueue`, `queue::answer`, `worker::work`, `worker::recover_orphans`,
`worker::event_tick`), `src/job.rs`'s `start`, `queue_triggered`,
`run_now`, `run_claimed`, `retry_job`, `recover_interrupted`, `drive`,
`src/report.rs`'s event log writer, `forge events` (`cli/stats.rs`),
`src/plugins.rs`'s supervision, `src/successor.rs`, and
`plugins/notify/notify.sh` as the reference `events` consumer.

What reaches `forge.db` at once. A daemon worker is one process with
one `Connection` behind a `std::sync::Mutex` (store/mod.rs:347-360), in
WAL mode with `busy_timeout=5000`; its `--jobs N` tasks and jobs share
that connection and serialize on the mutex. A config reload opens a
second connection in the same process (`Forge::reopen`, ctx.rs:229). A
successor worker is a second process for as long as the old one drains.
The web client, the portal, the TUI and every plugin reach the store
only by spawning `forge`, so each of their calls is a short-lived
process that opens the file, runs `migrate`, and writes or reads once.
Every store write outside `migrate` is an autocommit statement: the
store has exactly one explicit transaction (store/mod.rs:708), so every
multi-statement write below is a sequence another process can
interleave with or a crash can split.

### 3.1 Defects confirmed while reading

Fix these first; none needs a refactor. Ordered by what they cost.

1. **A task is claimable before `enqueue` has finished writing it, and
   `enqueue`'s second write puts a running task back in the queue.**
   `queue::enqueue` inserts the task `queued` (queue.rs:641), computes
   its journal arm and exploration draw (reading `experiment.toml` from
   disk), then writes the whole row again with `update_task`
   (queue.rs:660). `update_task` is `UPDATE ... WHERE id=?1` over 55
   columns with no state guard (store/tasks.rs:534-546). A worker whose
   claim lands between the two (`claim`, tasks.rs:669, is atomic but
   only against `state='queued'`) starts the task with the default
   journal and no draw; `enqueue`'s update then writes `state='queued'`,
   `worker_pid=NULL`, `started_at=NULL` over the running row, and the
   next claim by any worker runs the same task a second time in a second
   clone while the first is still going. `queue::answer` does the same
   with a wider window: `enqueue` returns the queued retry, then
   `set_decision_retry`, then `update_task(&n)` to copy the intake plan
   (queue.rs:1126-1130). Reproduction: hold a worker at the claim loop
   with a queued-task poll and enqueue from the CLI with a sleep injected
   after queue.rs:641; the task is claimed, then shows `queued` with an
   open `running` attempt.
   *Task:* make a task's first `queued` write its last: compute the
   journal arm and exploration draw before the insert (reserve the id or
   derive the draw from something other than the id), or insert it
   `blocked`/unclaimable and release it with a single guarded
   `UPDATE ... WHERE id=? AND state='<unclaimable>'`; give `answer`'s plan
   copy the same treatment. Add a store method for post-insert field
   writes that is guarded on `state IN ('queued','blocked')`, the way
   `set_task_fields` is, and stop calling `update_task` on a row another
   process may already own. A unit test: claim between insert and the
   follow-up write, and the task stays `running` with its worker.

2. **Orphan recovery is check-then-act across processes, so a starting
   worker can requeue a task or job another worker has just claimed.**
   `recover_orphans` (worker.rs:849-857) reads the running rows whose pid
   is dead (`orphans`, tasks.rs:933; `orphan_jobs`, jobs.rs:595), then
   acts on each id. `requeue` (tasks.rs:919-931) closes every `running`
   attempt of the task and sets it `queued` guarded only by
   `state='running'`, not by the dead pid it was chosen for. With two
   workers starting together (a successor beside a restarted unit, or two
   daemons), A requeues orphan T, B claims T and opens attempt k, and C,
   which listed T before A acted, then closes B's live attempt k as
   `agent_failed` and requeues T under B: T runs twice. Jobs are the same
   through `recover_interrupted` (job.rs:1652): with no effects recorded
   it appends a `recovery` step to the live run and `requeue_job`
   (jobs.rs:611) sets it `queued` again (guarded on `running` and no
   effects, not on the owner); with effects it files a question and
   `finish_job`s the live run `failed` (jobs.rs:413, unguarded), which the
   live run's own `finish_job` later overwrites. The same list-then-act
   runs only at worker start (worker.rs:876): a worker that dies while
   others keep running leaves its rows `running` until some worker
   process starts again.
   *Task:* guard every recovery write on the owner it was chosen for:
   `requeue(id, dead_pid, why)` updating attempts and task only
   `WHERE ... AND worker_pid IS ?dead_pid`, `requeue_job` and the
   recovery `finish_job` likewise on `jobs.worker_pid`, and do the pair in
   `requeue` in one transaction (`BEGIN IMMEDIATE`). Run
   `recover_orphans` on each pass of the claim loop, not only at start, so
   a live worker adopts a dead one's rows. Test: list orphans, claim one
   under a live pid, then requeue the listed id; the claim survives.

3. **A job effect performed but not yet recorded is repeated after a
   crash or a double-signal abort, and the evidence is deleted first.**
   An operation appends each effect to `$FORGE_EFFECT_LOG` as it performs
   it (`send-signal` sends, then appends,
   src/builtins/operations/send-signal.toml:11-12); `run_now` copies the
   new lines into `job_effects` only after the operation exits
   (job.rs:1195-1212). If the worker dies in between, or the operator's
   second signal aborts the run (`running.abort_all()`, worker.rs:1053;
   `kill_on_drop` kills the operation), `recover_interrupted` consults
   only the `job_effects` rows (job.rs:1654-1666), finds none, and
   requeues. The rerun's `run_now` then deletes the input directory and
   truncates `effects.log` (job.rs:1006-1011) before running the same
   step again: the message is sent twice and the line that proved the
   first send is gone. docs/JOBS.md, "When the worker dies", says
   recovery "cannot identify an external effect that happened before its
   row was recorded"; for any effect the operation logged, it can.
   *Task:* in `recover_interrupted`, read `<input_dir>/effects.log` and
   treat every line beyond the recorded rows as an effect: record it
   (marked recovered) and take the effects path (fail and ask) instead of
   requeueing. In `run_now`, never truncate an existing `effects.log` of
   a job that has run before; move it aside with the previous run's
   outputs. Test with a fake operation that appends a line and then
   blocks: abort the worker twice, and the job ends `failed` with the
   effect recorded, never `queued`.

4. **A queued job can be claimed before its input is written, and then
   runs on `{}`.** `forge job start` (job.rs:626 then 633),
   `queue_triggered` for message, webhook and event triggers (job.rs:956
   then 959) and `retry_job` (job.rs:1570 then 1573) all `create_job` a
   claimable row first and write `input.json` after. `claim_next_job` is
   one atomic statement and can run in that gap, in another process or in
   the same worker's loop while the retrying run finishes on another
   thread. `run_claimed` then reads the missing file as `"{}"`
   (job.rs:1604) and `run_now` writes that back over the directory
   (job.rs:1006-1008): a quote-by-text job runs with no contact and no
   text, and its verdict blames the workflow.
   *Task:* write the input before the row becomes claimable: create the
   job `scheduled` with a `due_at` in the past or a new `pending` state,
   write `input.json`, then flip it to `queued` with a guarded update; or
   store the input in the row. Make `run_claimed` fail the job on a
   missing `input.json` instead of defaulting to `{}`.

5. **A `retry:N` of a schedule-triggered job can never be created.**
   `retry_job` carries `trigger_kind` and `trigger_ref` over unchanged
   (job.rs:1551-1570, as documented: "a retry of a schedule-triggered job
   is still that slot's job"). The message, webhook and event unique
   indexes exempt retries with `AND retry_count = 0` (migrations.rs:514,
   539, 555); `jobs_schedule_slot` (migrations.rs:466) does not, so the
   retry's insert fails `UNIQUE constraint failed: jobs.project,
   jobs.workflow, jobs.trigger_ref`, which `apply_on_failure` prints to
   stderr and drops (job/flow.rs:69-72). Confirmed against SQLite with the
   index as written. No test starts a scheduled job with `retry:N`.
   *Task:* a forward migration that drops and recreates
   `jobs_schedule_slot` with `AND retry_count = 0`, and an e2e test: a
   schedule-triggered job with `on_failure = "retry:1"` that fails once is
   retried once.

6. **Concurrent event-log appends tear lines, rotation loses a
   generation, and neither is counted as dropped.** `append_log_with_limit`
   (report.rs:419-447) is called outside any lock, per process and per
   task. The line is written with `writeln!(f, "{v}")` on an unbuffered
   `File`; `serde_json::Value`'s `Display` writes token by token, so one
   line is many `write(2)` calls and `O_APPEND` keeps none of them
   together: two workers, or two tasks in one worker, interleave inside a
   line. Rotation is check-then-rename (report.rs:427-436): two writers
   that both see the file over 50 MB both rotate, and the second renames
   the first's fresh `.1` over `.2`, discarding a whole generation. Only
   an `open`/`write` error reaches `note_dropped` (report.rs:387-417), so
   `forge doctor`'s count is blind to both; and every job event is
   emitted as task 0 (job.rs:993), so all jobs' drops are one marker line.
   *Task:* serialize the line to a `String` and write it with one
   `write_all` of the complete line; take an `flock` on the log (or a
   sibling lock file) around the size check, rotation and append, so one
   process rotates and the rest reopen. Record job drops under the job id
   (a `job:<id>` marker), and count them in `doctor` separately.

7. **Byte offsets into the event log carry no generation, so every
   consumer resumes into the wrong file after a rotation.** `forge
   events` (cli/stats.rs:176-215) treats `len < pos` as "rolled" and
   restarts at 0 of the new file, without reading the old file's tail
   between `pos` and its end; it says nothing on stdout, so a consumer
   that counts bytes cannot know. `notify.sh` does exactly that
   (plugins/notify/notify.sh:42-46, 76): after a rotation its saved cursor
   is the old offset plus the new file's bytes. A plugin restarted then
   (a deploy's handover, `forge plugin restart`) is handed that cursor:
   larger than the new file, it replays the new file from 0 (duplicate
   notifications); smaller, it seeks into the middle of an unrelated line
   and emits the fragment. The worker's event trigger has the same shape
   (worker.rs:620-625) and keys its dedup on the offset
   (`trigger_ref = offset`, job.rs:811, unique at migrations.rs:555): a
   matching event that lands in a later file at an offset an earlier
   event already started a job for is taken for that event and never runs.
   *Task:* give the log a generation: write a header line (or a
   `events.gen` file) with a counter bumped on rotation, make `forge
   snapshot` return `generation:offset` and `forge events --since` accept
   it, read the tail of `events.jsonl.1` when the generation moved by one,
   and print a `resync` event when it cannot. Key event-triggered jobs on
   `generation:offset`. Update notify, signal, github-issues and
   statusline to store the cursor `forge events` reports rather than
   summing line lengths.

8. **Stopping a plugin signals only its leader; a shell plugin's
   pipeline outlives it and keeps acting.** `spawn_plugin`
   (plugins.rs:466-493) does not put the plugin in its own process group,
   and `stop_child` (plugins.rs:444-458) sends SIGTERM to the pid alone,
   SIGKILL only if the leader lingers. `notify.sh` runs
   `"$FORGE_BIN" events --follow | while read ...` with no trap: SIGTERM
   kills the shell, and the `forge events` follower and the `while`
   subshell keep reading and running the notify command (reproduced with
   the same pipeline shape: the leader died on SIGTERM, the loop kept
   consuming). Every restart, disable or worker handover adds one more
   orphaned consumer; under the successor handover the unit's cgroup
   lives on, so nothing reaps them. checks.rs:162 already does this right
   for operations with `process_group(0)`.
   *Task:* spawn plugins with `process_group(0)` and signal the group
   (`kill(-pgid, ...)`) in `stop_child`, SIGKILL to the group after the
   grace; a test with a plugin that runs `sleep 1000 | cat` finds no
   process of that group after `Supervisor::stop`.

9. **Every `forge work` runs its own copy of every plugin, and a deploy
   overlaps the two.** `work` starts a `Supervisor` unconditionally
   (worker.rs:880), `--once` included; nothing per home says a plugin
   already runs. During a release handover the old worker spawns the
   successor, then stops its plugins (worker.rs:902-905), draining them one
   at a time with up to ten seconds' grace each (plugins.rs:700-712),
   while the successor starts its own set on its first reconcile tick. For
   that window two copies share one `FORGE_PLUGIN_STATE` and one log: two
   notify processes each send the same message and race on `cursor`; two
   intake plugins poll the same inbox. `plugins-run/<name>.json` is
   last-writer-wins, so `forge plugin status` shows whichever supervisor
   wrote last.
   *Task:* make plugin supervision single per home: an `flock` on
   `plugins-run/<name>.lock` held by the supervising process for the life
   of the child, a supervisor that finds it held skipping that plugin and
   retrying on its reconcile tick, and `work --once` not supervising at
   all. Stop the draining worker's plugins concurrently, before the
   successor is spawned, so the lock passes rather than overlaps.

10. **A transient lock is treated as a broken environment.** Nothing
    retries `SQLITE_BUSY`: after `busy_timeout`'s 5 s the error propagates.
    Inside a task every store call is classified `env()` (engine.rs, 14
    sites), so one `database is locked` requeues the task and stops the
    worker claiming (worker.rs:153-158, then `stopping`, then exit
    non-zero). In the claim loop itself every store call is `?`
    (worker.rs:899-925): the error returns from `work`, which drops the
    `JoinSet` and aborts every in-flight attempt and job without requeueing
    them (and a job mid-operation lands in defect 3). The long writer that
    makes a 5 s wait plausible is Forge's own: a successor's `migrate`
    holds the write lock for a whole migration (the task-shape backfill
    rewrites every task row in one transaction) while the draining worker
    is still writing.
    *Task:* classify `rusqlite::ErrorCode::DatabaseBusy`/`DatabaseLocked`
    as retryable: retry the statement with backoff up to a minute inside
    `Store` before surfacing it, and in `work`'s loop log a store error and
    continue to the next pass instead of returning. Test with a second
    connection holding `BEGIN IMMEDIATE` for 6 s: the worker's claim waits
    it out.

11. **Two processes that open an old store together race the migration.**
    `migrate` reads `user_version` outside the transaction and starts it
    with a deferred `BEGIN` (store/mod.rs:699-708). Two opens at once (a
    successor and a `forge` spawned by the web client, right after an
    upgrade) both see version N; the second's first write waits for the
    first to commit and then applies migration N+1 again (`table already
    exists`, or `SQLITE_BUSY_SNAPSHOT` on the upgrade) and bails `migration
    to schema version N+1 failed`. For a CLI call that is one failed
    request; for the successor it is a failed start that `superseded`
    records as a failed release (successor.rs:80-84) and never retries.
    *Task:* open the migration with `BEGIN IMMEDIATE` and re-read
    `user_version` inside it, skipping entries another process already
    applied. Test: two threads open one pre-migration fixture at once and
    both succeed.

12. **A requeued job's first run is not reconciled.** After
    `recover_interrupted` requeues a job with no effects, the rerun's
    `job_steps` rows sit beside the first run's under the same `seq`
    values (job_steps has no run column), `forge job show` interleaves
    them, and the first run's `output_ref` files are gone
    (job.rs:1006). `finish_job` writes the rerun's own `total_cost` only
    (job.rs:1389), so a directive step the first run paid for is counted
    nowhere, and `[limits] budget_usd` is checked against the rerun alone.
    `recover_interrupted` emits no `JobFinished` on the path that ends
    the job `failed` (job.rs:1704-1710), so clients and event-triggered
    workflows never see that job end. A crash between its `ask` and its
    `finish_job` files a second question on the next start.
    *Task:* record a run number on `job_steps` (a migration adding `run`,
    bumped by `requeue_job`), keep each run's outputs under
    `<input_dir>/run-<n>/`, make `finish_job`'s cost the sum of the job's
    step rows, emit `JobFinished` from the recovery path, and finish the
    job before asking.

13. **`per_day` is check-then-insert.** `start` and `queue_triggered`
    count `jobs_started_since` and then `create_job` (job.rs:590-626,
    918-956) as two statements. Two triggers for one workflow, from two
    processes (the worker's message tick and a webhook delivery through
    the CLI), both pass at `per_day - 1`, and the workflow runs
    `per_day + 1` times that day.
    *Task:* do the count and the insert in one `BEGIN IMMEDIATE`
    transaction in a store method (`create_job_within(per_day)`), and
    return the refusal from there.

### 3.2 Read and found sound

- **The claims.** `claim` (tasks.rs:669), `claim_next_job` (jobs.rs:511,
  one `UPDATE ... WHERE id = (SELECT ...) RETURNING`), `withdraw`,
  `withdraw_job`, `set_task_fields` and `requeue_job` are single
  statements guarded on the state they expect; two processes can never
  both take one row. `claim_next`'s candidate list can be stale; the
  claim cannot.
- **Trigger dedup.** Message, webhook, event and schedule jobs are
  guarded by partial unique indexes (migrations.rs:466, 514, 539, 555),
  so a trigger two workers fire at once creates one job; the loser's
  `UNIQUE` error is caught (`start_webhook`) or printed and skipped
  (`event_tick`). Defects 5 and 7 are about what those keys are, not
  about the race.
- **Dependency release.** `release_or_reblock`, `block_dependents` and
  `reroute_dependents` (tasks.rs:330, 743, 782) read and then write per
  row without a transaction, but each write is guarded (`AND
  state='blocked'` / `'queued'`) and task states they read only move
  forward, so a stale read delays a release by one claim pass and never
  releases early. `reroute_dependents`' unguarded `after_json` write
  (tasks.rs:801) can lose a concurrent `forge task set --after` on the
  same row; that is an operator racing themselves, not worth a lock.
- **`requeue` split by a crash.** The two statements are idempotent: a
  crash between them leaves the task `running` under a dead pid, and the
  next recovery closes nothing and requeues it. Only the cross-process
  race (defect 2) is wrong.
- **`register_worker`** (workers.rs:21-44) is select-then-insert, and
  the parent and the successor both call it for the successor's pid. The
  bad interleaving leaves the successor's own row closed and the
  parent's open under the same pid and version; `superseded` compares
  ids and versions and gives the same answer either way, and
  `live_workers` filters by pid. Harmless as written.
- **The connection.** WAL with `busy_timeout` is right for short
  autocommit writers and many short readers: readers never block the
  writer and each statement commits alone. `synchronous` is left at
  WAL's default (`FULL`), so a committed row survives power loss. The
  mutex recovers from poisoning (`into_inner`), which is safe because no
  guard is ever held across an open transaction. `foreign_keys=ON` is
  set per connection, as SQLite requires. The one assumption WAL adds,
  that `forge.db` is on a local filesystem, holds for `FORGE_HOME` as
  deployed.
- **Blocking in async.** Store calls are synchronous under a
  `std::sync::Mutex` on a multi-thread runtime; a 5 s busy wait parks
  one runtime thread, not the process. Acceptable until defect 10 adds
  retries, which should then sleep outside the mutex.
- **Job recovery with recorded effects** does what docs/JOBS.md says:
  it never reruns, it names every recorded effect in the verdict and the
  question, and `requeue_job`'s `NOT EXISTS (job_effects)` guard is in
  the same statement as the state change, so a recorded effect can
  never be requeued past.
- **Deploys and the effects that are not jobs.** `start_deploy` and
  `finish_deploy` are single statements; a deploy row left unfinished by
  a crash is visible as such (`finished_at` NULL) and is not retried by
  anything. Out of this slice's scope beyond that.
- **Plugin restart policy and handed environment.** Backoff, uptime
  reset, `restart_gen` and the reconcile tick do what docs/PLUGINS.md
  says. A plugin is handed `FORGE_BIN` through `<bin>/current`
  (binary.rs:7-25), so one restarted mid-deploy calls whichever release
  `current` names at that moment. The successor migrates the schema in
  `Forge::open` before `take_over` flips `current` (successor.rs:57-60),
  so a plugin restarted inside that window runs the old binary against
  the new schema and every call is refused (`schema version ... is newer
  than this forge`); it exits non-zero, backs off one to two seconds,
  and comes back on the new binary. Self-healing, so not a defect; worth
  knowing when reading a plugin log from a deploy.
