# Third architectural review: the kernel's concurrent paths (2026-09-26)

Each section is one cold read of one slice of the kernel, confirmed at
its lines. A defect names what it does and how to reproduce it, and ends
with the text of the task that fixes it. What was read and found sound
is listed so the next reader does not read it again.

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
