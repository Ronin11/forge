# Landed-work audit, batch 18 (forge, tasks 778-794)

Citation repair on 2026-10-03, against source tree
`a4941782da775bf85f8a7224938b3dc58f678278`. This batch contains tasks
778, 781, 782, 784, 787, 789, 790, 792, 793 and 794 only.
All implementation and regression-test citations below were re-read in that
tree. Symbol names accompany the locations so later moves can be traced.
The one historical engine range in task 790 is explicitly pinned to its
historical revision; it is not a location in today's engine.

The verdicts describe the implemented fixes. Test citations identify coverage,
not executions during this repair. The predecessor's test-run, manual-repro,
file-size and merge-parent claims are not used as current evidence. No Rust
suite was rerun for this documentation-only repair; Forge runs the declared
checks after submission. Task 793 retains a separate operational verification
follow-up below.

| task | verdict | evidence | what is missing |
|---|---|---|---|
| 778 text-only refusal holds the provider | done | `hold_text_only` sets `rate_limited` and supplies a five-minute full-window sample only if none exists (`src/agent/refusal.rs:9-14`). Claude's result reader calls `read_claude_error` (`src/agent/claude.rs:81`), which calls the helper for rate-limit text (`src/agent/refusal.rs:58-70`); this is an indirect call, not a call from `src/agent.rs`. Jev also calls it (`src/agent/jev.rs:555`). Codex and Copilot supply window samples in their rate-limit branches (`src/agent/codex.rs:71-72`, `src/agent/copilot.rs:155-156`). The directive increments consecutive refusals, exits when the count **exceeds** the limit of five, and resets the count on other outcomes (`src/engine/step/bookkeeping.rs:81-98`); `refusal_exhausted` returns an uncounted failure (`src/engine/step/bookkeeping.rs:120-130`). Regression coverage: provider hold in `tests/e2e/worker.rs:1130-1159`, bounded retries in `tests/e2e/worker.rs:1165-1189`. | nothing |
| 781 landing pushes the base before the task branch | done | `integrate` pushes the candidate to the base first and handles failure before reaching the task-branch push (`src/landing.rs:585-623`). `on_base_push_failure` checks whether the remote moved and bounds retries (`src/landing/round.rs:37-86`). Conflict recovery adopts the clone's tree before placing the branch (`src/landing.rs:411-414`); the verification-failure path also adopts it (`src/landing.rs:478`). Regression coverage: `the_base_moving_twice_during_merged_tree_verification_conflicts_once_and_still_lands` (`tests/e2e/landing_rewind.rs:13`). | nothing |
| 782 deploy-on-landing drops its errors | done | `deploy_on_landing` lives in the effects module and emits a task note naming the target and error returned by `deploy::run` (`src/landing/effects.rs:46-74`). `non_self_src` resolves a landing's SHA in the kernel repository, retaining the registered checkout for operator deploys (`src/deploy.rs:329-352`). After `start_deploy` (`src/deploy.rs:564`), the deploy body funnels errors through `record_deploy_error` (`src/deploy.rs:742`), which finishes an open row with `check_ok: false` and the error reason (`src/deploy.rs:383-404`). Regression coverage: an unknown on-landing method still lands, emits the failure, and creates no deploy row (`tests/e2e/deploy_errors.rs:11-74`). | nothing |
| 784 Store::open still races on journal_mode=WAL | done | `Store::open` installs `busy_timeout` before retrying `journal_mode=WAL`, then enables foreign keys (`src/store/mod.rs:402-408`). `migrate` takes an immediate transaction and rechecks the schema version before applying migrations (`src/store/mod.rs:838-865`). Regression coverage: `two_threads_racing_the_same_pending_migration_both_succeed` starts two opens behind a barrier, joins both successfully and checks the final schema version (`src/store/mod.rs:949-971`). | nothing |
| 787 presence.sh never clears an active-only quota on idle | done | `apply` chooses the active or idle quota and always calls `systemctl` with either `CPUQuota=${quota}%` or `CPUQuota=infinity` (`plugins/presence/presence.sh:50-67`). Regression coverage: `apply_idle_clears_a_quota_that_only_the_active_state_configures` runs active then idle with only `ACTIVE_QUOTA=200` configured, asserting both quota values in the fake systemctl log (`tests/e2e/presence.rs:209-246`). | nothing |
| 789 doctor shadowing row hides a failed diff as 0 diff lines | done | `Shadow.diff` carries `Result<String, String>` (`src/workflows/shadow.rs:63-66`). `diff_of` creates a unique temporary file and accepts only exit codes 0 and 1; other codes and signals produce errors (`src/workflows/shadow.rs:178-205`). `state_key` combines HEAD and action-file mtime (`src/workflows/shadow.rs:230-245`); `scan` caches only batches whose diffs all succeeded (`src/workflows/shadow.rs:301-316`). `report` checks the result and prints `diff failed` on error (`src/workflows/shadow.rs:423-429`), rather than using the zero fallback in `diff_lines` (`src/workflows/shadow.rs:70-81`). Coverage: concurrent diffs (`src/workflows/shadow.rs:601`), retry after a failed scan (`src/workflows/shadow.rs:627`), and a killed diff reported as failure rather than zero lines (`tests/e2e/shadowing.rs:279-322`). | nothing |
| 790 REVIEW-4 finding 2's citation was wrong | done | The corrected document says 533 lines and 796-1328 (`docs/REVIEW-4.md:1093-1095`). These describe historical `run_directive_step`, not the current engine. Reading `git show 1a612c4:src/engine.rs` confirms the function declaration at historical line 796 and closing brace at historical line 1328; the inclusive length is 533. | nothing |
| 792 flaky portal conversation-thread ordering test | done | `ConvMessage` and `ConvReply` have row ids (`portal/src/main.rs:499-518`). `render_conversation` constructs timestamp/kind/id entries for messages, replies and questions and sorts on all three keys (`portal/src/main.rs:561-618`, specifically `portal/src/main.rs:613`). Regression coverage: `three_messages_and_one_question_render_as_one_thread_in_order` repeats the request/assert cycle 20 times and compares each rendered thread to the previous one (`portal/tests/conversation.rs:195-212`), then checks the embedded snapshot (`portal/tests/conversation.rs:222-226`). | nothing |
| 793 the 789 shadow-scan test leaves core dumps | done | The fake git uses `kill -TERM $$` (`tests/e2e/shadowing.rs:291`), replacing the core-dumping BUS signal with a terminating signal whose default action does not dump core. The test still asserts `diff failed` and rejects `0 diff line(s)` (`tests/e2e/shadowing.rs:312-313`). `diff_of` reports signal termination generically (`src/workflows/shadow.rs:200-203`). The implementation fix is present; no systemd coredump inventory or full-suite execution was performed during this citation repair. | Operational verification remains: follow-up 793-V below. |
| 794 event log carries no generation across rotation | done | `Cursor` formats and parses generation/offset pairs (`src/report/log.rs:7-27`). `append` increments the generation, writes its header and rotates the files (`src/report/log.rs:66-88`); `read` drains the preceding generation when available or requests resync (`src/report/log.rs:106-138`). Snapshot exposes the string cursor (`src/cli/stats.rs:175-183`); events parses it, emits resync when needed and adds cursors to events (`src/cli/stats.rs:206-233`), as documented (`docs/CLIENT.md:1361-1384`). `StartEvent.offset` is a string (`src/job.rs:55`), used as the event job's trigger reference (`src/job.rs:524-540`). `event_tick` reads snapshots and batches and persists cursor progress (`src/worker.rs:625-690`). Migration SQL converts old positions and event job references to generation zero (`src/store/migrations.rs:689-692`). Plugins persist reported cursors: notify (`plugins/notify/notify.sh:64`, `plugins/notify/notify.sh:135`), signal (`plugins/signal/signal.sh:332-333`), github-issues (`plugins/github-issues/github-issues.sh:113`, `plugins/github-issues/github-issues.sh:141`), statusline (`plugins/statusline/statusline.sh:102-103`). Coverage: legacy migration (`src/store/events.rs:69`), generation-aware worker deduplication (`src/worker/tests.rs:732`), rotation (`tests/e2e/event_cursors.rs:21`), resync (`tests/e2e/event_cursors.rs:54`), plugin restart (`tests/e2e/event_cursors.rs:73`) and killed-follow resumption (`tests/e2e/event_cursors.rs:138`). | nothing |

Follow-up 793-V (operational verification, not a missing code change): on a
systemd-booted host, record the coredump inventory, run the declared suite using
`forge-test`, then compare the inventory for new dumps caused by the fake git
in the shadowing test. Record the suite result and inventory comparison. This
is a documented follow-up, not a claim that an external Forge task was filed.

```json
[
  {"task": 778, "verdict": "done", "missing": ""},
  {"task": 781, "verdict": "done", "missing": ""},
  {"task": 782, "verdict": "done", "missing": ""},
  {"task": 784, "verdict": "done", "missing": ""},
  {"task": 787, "verdict": "done", "missing": ""},
  {"task": 789, "verdict": "done", "missing": ""},
  {"task": 790, "verdict": "done", "missing": ""},
  {"task": 792, "verdict": "done", "missing": ""},
  {"task": 793, "verdict": "done", "missing": "Operational verification only: documented follow-up 793-V requires a systemd-host suite run and before/after coredump inventory."},
  {"task": 794, "verdict": "done", "missing": ""}
]
```
