# Landed-work audit, batch 12 (forge, tasks 639–660)

Audited on 2026-10-04 against `1c0019e` (the predecessor audit branch with
current base `c71441e` merged). This replaces the predecessor's all-done
assessment: **3 done, 7 partial**. Passing fixtures are evidence for what they
assert, not proof that every sentence of the task was implemented. No code,
configuration, or tests were changed.

## Change provenance

For merge commits whose subject starts `Merge main into`, the own-change
range is the second parent to the merge, as requested. The other ranges use
the supplied base. These are the ranges inspected, not diffs against today's
unrelated work:

| task | own-change range |
|---|---|
| 639 | `fdf057e9d750^2..fdf057e9d750` |
| 642 | `2eb5c41a20b7..76c9c84af0a2` |
| 644 | `e9a03d934d89^2..e9a03d934d89` |
| 645 | `d46f4339b220^2..d46f4339b220` |
| 650 | `31845b5ab44c..58779db0d5ca` |
| 651 | `219e68b54240^2..219e68b54240` |
| 652 | `c8505ecda3be^2..c8505ecda3be` |
| 657 | `219e68b54240..23548406c8af` |
| 659 | `31845b5ab44c^2..31845b5ab44c` |
| 660 | `d74028321d44..0c9f5cc06aa3` |

Current paths were checked after the reorganizations: `git log --follow`
identifies `7be833d` for `src/cli/web.rs` and `ba304e7` for
`src/engine/needs.rs`. `git log -S 'cache_paths: &[PathBuf]' --
src/environment.rs` identifies `eef2cec` (task 871, merged at `1f412bd`):
it deliberately replaced task 650's arbitrary `~/.cache` ceiling with the
operator's explicit table. That aspect is superseded; the whole task remains
partial because the uncovered-need routing omission is independent of it.
Task 650 also extends task 659's original uncovered-host behavior rather than
replacing its recognizer and automatic-grant implementation.

## Verdicts

All test names below passed in the scoped run recorded under Validation.
Source-derived counterexamples are identified as such; they were not added
as tests or represented as executed failures.

| task | verdict | evidence on current tree | what is missing |
|---|---|---|---|
| 639 | partial | `src/config/home.rs:164` and `src/config/home.rs:580` define/load the dependency cache; `src/sandbox.rs:933` binds it read-only; `src/doctor.rs:209` warns if absent. `src/ctx.rs:310` restricts model trust to no declared hosts, without testing cache presence. `src/engine/land.rs:51` returns Unverified naming trust; `src/landing.rs:1166` gates automatic landing, and `src/landing.rs:1249` invokes landing effects whose assessment is at `src/landing/effects.rs:35`. `README.md:263` documents both cache and fallback. **PASS:** `trust::a_public_task_ends_unverified_with_its_branch_pushed_and_forge_land_lands_it` (`tests/e2e/trust.rs:297`) proves pushed/unverified then manual landing; it does not exercise registry fallback. | Absent-cache setup fallback is not implemented: the same cleared egress policy reaches checks (`src/checks.rs:225`), and the only cache-dependent sandbox branch is the mount. README promises more than the code provides. |
| 642 | partial | `portal/src/main.rs:424` renders initiative outcome and done/total; `portal/src/main.rs:442` renders landing text, live time and screenshot. `src/view/projects.rs:1135` selects a successful, non-rolled-back deploy and its screenshot; `portal/src/main.rs:786` restricts screenshot lookup. **PASS:** `done_shows_what_changed_when_it_went_live_and_the_look_screenshot` (`portal/tests/done.rs:147`, fixture `portal/tests/done.rs:43`, snapshot `portal/tests/snapshots/done.txt:1`) checks two supplied landings, one deployed, and 2 of 3 done. | Not every landed task is represented: `src/view/projects.rs:1157` emits only settled initiative aggregates and `src/view/projects.rs:1177` excludes all initiative tasks from individual entries. A landed task in an unfinished initiative has no Done entry; settled initiatives lose each task's own title/evidence. The mocked JSON snapshot bypasses this producer. |
| 644 | done | `src/cli/web.rs:60` requires an executable regular file; `src/cli/web.rs:69` searches beside forge then PATH, passes bind, and uses exec with the named-location error. No web implementation is imported. Operator docs: `docs/CLIENT.md:520`; README command: `README.md:61`. **PASS:** `web::forge_web_serve_runs_forge_web_from_path_with_the_bind_flag` (`tests/e2e/web.rs:5`) and `web::forge_web_serve_skips_a_non_executable_forge_web_on_path` (`tests/e2e/web.rs:56`), including the supervisor's two-PATH-directory regression. | None. Boundary source remains protected and unchanged; its tests and the historical full-suite/clippy instructions were not rerun in this scoped audit. |
| 645 | done | `tui/src/lib.rs:1109` renders outcome, held reason, elapsed time, task states/costs, refusals, rulings, questions and deploys; `tui/src/lib.rs:1082` draws the block cost bar. `tui/src/lib.rs:403` calls initiative set for budget/stop-after. `reload_initiative` (`tui/src/lib.rs:382`) preserves screen and is called at `tui/src/lib.rs:164` and `tui/src/lib.rs:288`. **PASS:** `a_held_initiative_renders_its_report` (`tui/tests/snapshots.rs:287`), `refresh_re_reads_the_open_initiative_report` (`tui/tests/snapshots.rs:301`, refresh and snapshot, atomic fake replacement, also refresh while off-screen), and `the_budget_and_stop_after_keys_call_initiative_set` (`tui/tests/snapshots.rs:334`). Held snapshot: `tui/tests/snapshots/initiative_held.txt:1`. | None in the supplied task text and supervisor correction. The supplied tail is truncated; no unseen requirement is inferred. The set-key fixture exercises budget; the shared stop-after branch is verified in source. |
| 650 | partial | `src/env_supervisor.rs:43` supplies typed need/evidence/table/ceiling; `src/env_supervisor.rs:93` checks lineage budget and holds approvals to the ceiling. `src/engine/needs.rs:89` applies the approval or blocks on a reasoned denial; `src/environment.rs:455` records attribution. Host exclusions and repository deny: `src/environment.rs:378`; current cache ceiling: `src/environment.rs:430`. Docs: `docs/OPS.md:542`. **PASS:** `environment::an_unlisted_registry_host_is_approved_by_the_supervisor_and_the_run_repeats` (`tests/e2e/environment.rs:80`), `environment::a_wildcard_is_denied_whatever_the_supervisor_says_and_the_question_names_why` (`tests/e2e/environment.rs:103`), `environment::a_denial_reaches_the_operator_with_the_supervisors_reason` (`tests/e2e/environment.rs:118`) and `environment::the_repository_can_deny_a_host_the_supervisor_would_approve` (`tests/e2e/environment.rs:132`). Ceiling unit tests at `src/environment.rs:688`, `src/environment.rs:715`, `src/environment.rs:732` also pass. | `src/env_supervisor.rs:80` excludes Binary and Toolchain outright, so an uncovered typed need of either kind returns Left at `src/engine/needs.rs:106` without the required supervisor denial. This is explicitly documented at `docs/OPS.md:563`. Cache-ceiling replacement by task 871 is intentional, not a missing restoration. |
| 651 | partial | `src/cli/statistics.rs:415` renders table/JSON; `src/store/questions.rs:54` gathers windowed questions and resolutions; `src/view/questions.rs:137` defines counts, median wait and attention fields, computed at `src/view/questions.rs:176`. Docs: `docs/CLIENT.md:1251`. **PASS:** `questions::three_supervisor_answered_demotions_that_landed_are_do_it_as_stated` (`tests/e2e/questions.rs:59`, three supervisor-answered review demotions with landed retries) and all seven `view::questions::questions_tests` tests: `an_answer_that_only_restates_the_ask_is_as_stated` (`src/view/questions_tests.rs:7`), `an_answer_that_adds_a_decision_is_not_as_stated` (`src/view/questions_tests.rs:22`), `the_landed_retry_with_only_the_answer_appended_is_as_stated` (`src/view/questions_tests.rs:37`), `a_retry_that_changed_anything_else_is_not_as_stated` (`src/view/questions_tests.rs:58`), `a_demotion_answered_with_one_fix_that_landed_is_as_stated` (`src/view/questions_tests.rs:103`), `withdrawn_and_open_questions_are_never_as_stated` (`src/view/questions_tests.rs:109`), `the_document_counts_kinds_medians_and_prices_operator_attention` (`src/view/questions_tests.rs:119`). | The classifier is broader than repeating the ask: `src/view/questions.rs:50` returns true for any answer containing an approval phrase, even negated or followed by a new decision. Source-derived counterexample: 'Do not proceed; use a different database'. The negative fixtures never combine an approval phrase with added instructions. |
| 652 | partial | `src/supervisor.rs:407` files the same-lineage follow-up, enforces the budget and records `demotion-as-task` (`src/supervisor.rs:441`); the worker invokes it before supervision (`src/worker.rs:139`). `src/store/questions.rs:113` reads decisions for question accounting. Docs: `docs/WORKFLOWS.md:73`. **PASS:** `questions::a_demotion_with_a_reproduction_files_a_follow_up_that_lands` (`tests/e2e/questions.rs:106`) proves follow-up, decision, inherited branch file and eventual success; `questions::a_demotion_whose_follow_up_fails_stays_blocked` (`tests/e2e/questions.rs:175`) and `questions::a_demotion_that_asks_something_still_blocks_with_the_question` (`tests/e2e/questions.rs:195`) cover failure/question paths. | The reproduction predicate at `src/supervisor.rs:367` rejects valid inline commands with one token (requires two whitespace-separated words), and rejects every '?' even inside a command rather than only operator questions. Source-derived example: 'Running `false` exits 1, expected 0' fails the predicate. Existing e2e uses a two-word command. |
| 657 | partial | Event recording/filtering: `tui/src/activity.rs:74`, `tui/src/activity.rs:343`; query flags and before cursor: `tui/src/activity.rs:137`, `tui/src/activity.rs:270`; event subscription: `tui/src/lib.rs:758`. **PASS:** `a_fixture_event_stream_renders_its_kinds` (`tui/tests/snapshots.rs:687`, snapshot `tui/tests/snapshots/activity_feed.txt:1`) and `a_query_renders_the_verb_call_and_pages_by_before` (`tui/tests/snapshots.rs:733`, snapshot `tui/tests/snapshots/task_list_query.txt:1`). | Actual step and live turns/cost are absent. `Running` (`tui/src/activity.rs:36`) has attempt numbers but no step; the STEP cell at `tui/src/activity.rs:302` formats those numbers. Only AgentDone sets turns/cost (`tui/src/activity.rs:90`), then AttemptDone removes the row. `seed_running` (`tui/src/activity.rs:176`) inserts empty defaults. Snapshot explicitly expects 'attempt 2 of 3', so it does not prove step or live spend. |
| 659 | partial | `src/environment.rs:84` recognizes host/binary/toolchain/cache needs; defaults are at `src/environment.rs:37`, policy at `src/environment.rs:294`. `src/engine/needs.rs:78` applies/records grants and `src/engine/step.rs:99` reruns operations without a retry. `src/ctx.rs:353` applies scoped grants; doctor lists seven days at `src/doctor.rs:1032`; docs at `docs/OPS.md:503`. **PASS:** `environment::a_covered_host_is_granted_and_the_run_repeats_with_no_question_and_no_retry_spent` (`tests/e2e/environment.rs:22`) and `environment::a_host_the_table_does_not_cover_still_fails_as_before` (`tests/e2e/environment.rs:50`). Recognizer tests `a_proxy_refusal_names_the_host`, `a_tools_403_line_with_a_url_names_the_host`, `a_missing_browser_cache_names_the_path`, `a_missing_binary_or_toolchain_is_typed`, `ordinary_failures_and_questions_are_nothing`, `a_question_is_read_the_same_way` (`src/environment.rs:527` onwards) all pass. | The requested evil.example blocked-question e2e is not present: the existing test asserts state failed (`tests/e2e/environment.rs:55`) and an operation-setup failure. It proves no accidental grant, but not the requested terminal question. Task 650 adds supervisor handling when enabled; the supervisor-disabled fixture still fails outright. |
| 660 | done | `tests/fn_length.rs:648` walks tracked Rust files including test modules, enforces 120 or the allowlisted ceiling, reports name/length/ceiling, and rejects stale entries at `tests/fn_length.rs:685`. Allowlist/reasons: `tests/fn_length.rs:10`; blanking/brace scanner: `tests/fn_length.rs:496`, `tests/fn_length.rs:586`. Rule beside file size: `CONTRIBUTING.md:92`. **PASS:** `tracked_rust_functions_stay_within_their_line_limits` plus `scanner::braces_in_strings_and_raw_strings_do_not_count`, `scanner::char_literals_and_lifetimes`, `scanner::comments_are_ignored`, `scanner::nested_closures_match_arms_and_inner_fns`, `scanner::bodyless_fns_and_fn_pointer_types_are_skipped` (`tests/fn_length.rs:713` onwards): six tests total. | None. The documented measure is signature through closing brace (stricter than body only); shrinking allowlist policy is documented and stale entries are executable checks. |

## Validation

Ran only the task-related filters below through `forge-test`; **exit 0,
51 passed, 0 failed** (23 kernel unit tests, 16 e2e, 6 function-length tests,
1 portal snapshot, 5 TUI snapshots). Other workspace targets selected zero
tests. The environment module filter includes related later regression tests;
no full suite was run. An environment e2e has a capability-dependent early
return, so its harness success is not claimed as proof of sandbox support.
Full log: `.git/forge-test/89a537fcfef4b06c.log` (local, not committed).

```sh
forge-test cargo test --workspace -- \
  a_public_task_ends_unverified_with_its_branch_pushed_and_forge_land_lands_it \
  done_shows_what_changed_when_it_went_live_and_the_look_screenshot \
  forge_web_serve a_held_initiative_renders_its_report \
  refresh_re_reads_the_open_initiative_report \
  the_budget_and_stop_after_keys_call_initiative_set \
  environment:: questions:: view::questions \
  a_fixture_event_stream_renders_its_kinds \
  a_query_renders_the_verb_call_and_pages_by_before \
  tracked_rust_functions_stay_within_their_line_limits \
  braces_in_strings_and_raw_strings_do_not_count char_literals_and_lifetimes \
  comments_are_ignored nested_closures_match_arms_and_inner_fns \
  bodyless_fns_and_fn_pointer_types_are_skipped
```

No claim is made that the repository-wide fmt, clippy, setup or test checks
pass; those are left to Forge as requested. The initial `forge-test --help`
probe exited 2 because this wrapper treats its first argument as an executable;
it ran no tests. The successful invocation above is the verification result.
The missing paragraphs below are ready to file as follow-up tasks; this audit
neither changes implementation nor claims those follow-ups have been filed.

```json
[
  {
    "task": 639,
    "verdict": "partial",
    "missing": "Implement the promised setup-only declared-registry fallback when [sandbox] dependency_cache is absent, while keeping agent attempts at model-only egress; currently allow_egress clears declared rules regardless of cache presence. Add a sandboxed regression that exercises setup with and without the cache and verifies the agent cannot use the fallback. Reconcile README.md's fallback claim with the implemented behavior."
  },
  {
    "task": 642,
    "verdict": "partial",
    "missing": "Expose every landed task in Done, including tasks in an unfinished initiative and the individual tasks of a settled initiative, using their title or first sentence and their own deployment evidence. Currently initiative tasks are excluded from the task loop and only settled initiatives get an aggregate outcome entry. Add a store-backed portal fixture with both unfinished and settled initiatives; the current two-landing snapshot uses prebuilt JSON and cannot detect this omission."
  },
  {
    "task": 644,
    "verdict": "done",
    "missing": ""
  },
  {
    "task": 645,
    "verdict": "done",
    "missing": ""
  },
  {
    "task": 650,
    "verdict": "partial",
    "missing": "Route recognized uncovered binary and toolchain needs through a bounded supervisor denial rather than returning them to the original failure/question path. Include the typed need, evidence, policy and code-enforced ceiling, and only send the operator a reasoned yes/no denial. Add regressions for both need kinds. Preserve task 871's later restriction of cache grants to the operator's explicit cache_paths table rather than restoring the superseded arbitrary ~/.cache ceiling."
  },
  {
    "task": 651,
    "verdict": "partial",
    "missing": "Make the do-it-as-stated classifier reject answers that introduce a decision even when they contain phrases such as go ahead or proceed, and reject negated approvals. The current substring early return classifies 'Do not proceed; use a different database' as as-stated regardless of the question or landed retry. Add positive and negative fixture answers covering approval phrases with extra instructions, negation and repeated-word alternatives; retain the independent exact appended-answer-and-landed rule."
  },
  {
    "task": 652,
    "verdict": "partial",
    "missing": "Recognize one-word inline reproduction commands and distinguish question marks in command literals from questions to the operator. Currently 'Running `false` exits 1, expected 0' is not routed as a task, and an otherwise valid reproduction containing a URL query string is rejected by the blanket question-mark check. Add focused classifier and end-to-end cases while retaining same-lineage branch inheritance, budget enforcement and demotion-as-task decisions."
  },
  {
    "task": 657,
    "verdict": "partial",
    "missing": "Populate the activity running-attempt strip with the actual workflow step and live turns/cost so far. Currently STEP displays attempt n of m; turns and cost are only filled by AgentDone and the row disappears at AttemptDone. Seed these fields for attempts already running when the TUI opens, and add a snapshot before agent completion proving live updates and the step name. Keep the existing event filters and before-cursor query paging."
  },
  {
    "task": 659,
    "verdict": "partial",
    "missing": "Complete the specified uncovered-host failure terminal behavior: the evil.example setup fixture currently ends failed (and explicitly asserts failed), whereas the task requested a blocked question. Add a regression proving the uncovered need ends in a crisp actionable question when no grant is available, while preserving task 650's supervisor-first route when enabled and the no-retry automatic-grant path. Do not treat the existing passing failed-state assertion as proof of the requested blocked behavior."
  },
  {
    "task": 660,
    "verdict": "done",
    "missing": ""
  }
]
```
