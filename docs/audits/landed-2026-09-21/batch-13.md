# Landed-work audit, batch 13 (forge, tasks 665–678)

Audited on 2026-10-04 against `95abffe` (the inherited audit branch with
current main merged). Scope is the ten supplied tasks: 665, 666, 667,
668, 670, 672, 674, 675, 677, 678. Verdicts: eight done, two partial.
No implementation or test changes were made. The missing paragraphs below
are follow-up task specifications; this audit does not claim they have
already been filed in an external task store.

## Change provenance and verification

For the four subjects beginning literally `Merge main into` (666, 667,
672, 678), read the second-parent diff. For every other
subject, read the supplied base-to-landed diff. In particular, 668 and
677 say `Merge forge/main…`, so the literal rule selects base-to-landed;
their first-parent work also identifies the task-specific additions.
The earlier audit incorrectly counted these merge categories.

| Task | Change compared |
|---|---|
| 665 | `git diff 58779db0d5ca 94b190d78a44` |
| 666 | `git diff 232fc127eac5^2 232fc127eac5` |
| 667 | `git diff d394f069b001^2 d394f069b001` |
| 668 | `git diff 58779db0d5ca fb8bbe51e773`; own first-parent commits `7c0e484`, `967c009`, `6318354` |
| 670 | `git diff d394f069b001 d9a77b64f106` |
| 672 | `git diff 7ce45ac2535f^2 7ce45ac2535f` |
| 674 | `git diff 171a099ddb88 ab29d9b43240` |
| 675 | `git diff 63bb5b12f8e1 a73d2ae5683c` |
| 677 | `git diff 63bb5b12f8e1 e0b911ad030f`; own first-parent commit `e48dce6` |
| 678 | `git diff 171a099ddb88^2 171a099ddb88` |

Current source, named tests and requested documentation were checked against
those changes. Moves were followed through history: the config template
moved to `src/config/home.rs` (`dd11142`), doctor binaries to
`src/doctor/binaries.rs` (`de8e9ae`), verification entry points to
`src/verify/directive.rs` (`6662787`), and landing settlement to
`src/engine/terminal.rs` (`e39cba3`). These are retained implementations,
not superseding replacements of the task requirements.

Only task-specific tests were run, through `forge-test`. All passed on
this tree: 22 unit tests and 19 e2e tests, zero failures or ignored tests.
The sandbox opt-out `FORGE_TEST_NO_SANDBOX` was unset; the old-bwrap e2e
was not deliberately bypassed. Exact executed test names are in the logs
and named in the table below. The three invocations were:

- `forge-test cargo test --locked --bin forge -- attempt_model pricing::tests argument_policy_tests binary::tests bwrap_versions_parse_and_gate_overlay a_fake_bwrap_version_is_probed without_overlay_support_caches_are_not_bound workflows::library::tests --test-threads=1`: 20 passed, exit 0; `.git/forge-test/f8b8c806fefd1ee1.log`.
- `forge-test cargo test --locked --test e2e -- <the 19 e2e names listed below> --test-threads=1`: 19 passed, exit 0; `.git/forge-test/03504daf02b878a9.log`. All names were passed together as separate libtest filters, not as a module-wide or full-suite filter.
- `forge-test cargo test --locked --bin forge -- reprice_moves_a_claude_rows_cli_figure_aside_and_prices_the_cache preamble_orders_untrusted_data_then_permission_then_outcome_then_protected --test-threads=1`: 2 passed, exit 0; `.git/forge-test/42bb2e06b9be48e7.log`.

The full workspace suite, clippy, fmt and setup were not run in this audit;
Forge runs the declared checks afterward. Thus “done” below records the
requested behavior/docs and current targeted-test success, not a fresh
full-suite result. The inherited explanation attributing past suite failures
to disk-capacity flakiness is removed: the supplied failure records do not
establish that cause. Task 672's supplied supervisor answer is truncated;
this audit covers the original rules and the visible prerelease instructions,
without asserting anything about the omitted text.

## Findings

All test names in this table passed in the runs above. Paths and line
numbers refer to the audited current tree, not the landing snapshots.

| Task | Verdict | Evidence | What is missing |
|---|---|---|---|
| 665 — settle blocked lineage predecessors | done | `src/queue.rs:527` walks `retry_of`, checks the predecessor is still blocked and its decision retried into the child, and calls `withdraw` with `superseded by <landed id>` and author `forge`. `src/queue.rs:470` records the decision and withdrawal. Both automatic landing (`src/engine/terminal.rs:38`) and manual landing (`src/landing.rs:1250`) invoke it. Doctor derives queue counts from current store state (`src/doctor.rs:697`), and its blocked-question scan uses `store.blocked` (`src/doctor.rs:743`). E2e `tests/e2e/questions.rs:106` `a_demotion_with_a_reproduction_files_a_follow_up_that_lands` checks withdrawal, decision author and disappearance from requests; `tests/e2e/questions.rs:175` `a_demotion_whose_follow_up_fails_stays_blocked` checks the negative case; `tests/e2e/supervisor.rs:20` `the_supervisor_answers_a_question_with_citations_and_the_answer_lands` checks the supervisor retry. All three pass. | None. |
| 666 — claude provider model precedence | done | `src/attempt.rs:439` preserves the supervisor's configured model, gives a non-built-in claude provider's model precedence over defaults, and preserves pinned models; `src/attempt.rs:464` recognizes flag/step pinning and `src/attempt.rs:480` records provider selection as `operator`. Six passing `attempt::tests::attempt_model_*` tests (`src/attempt.rs:852` onward): `keeps_the_supervisors_own_model`, `a_claude_providers_own_model_beats_the_tasks_default`, `a_pinned_model_beats_the_claude_providers`, `a_claude_provider_without_a_model_takes_the_tasks`, `the_builtin_anthropic_keeps_the_tasks_model`, `another_runner_takes_the_providers_model_never_the_tasks` (each prefixed `attempt_model_`). E2e `tests/e2e/economist.rs:140` `a_claude_providers_own_model_wins_unless_the_task_pinned_one` passes, checking opus versus pinned sonnet launch argv. Routing documentation: `docs/ECONOMIST.md:119`; provider template comment: `src/config/home.rs:344`. | None. |
| 667 — claude list pricing and CLI comparison | done | `src/pricing.rs:26` enables pricing only for a priced claude provider; `src/pricing.rs:39` includes input, output, cache reads (default input/10) and cache creation (input price); `src/pricing.rs:51` preserves the CLI figure. `src/store/migrations.rs:649` adds `cli_cost_usd`; `src/store/attempts.rs:520` reprices old nonzero claude rows without an earlier reprice/CLI comparison, saves the old figure and marks `repriced_at`. Passing `pricing::tests` at `src/pricing.rs:77`: `cache_reads_default_to_a_tenth_of_input_and_creation_is_at_input`, `an_explicit_cache_read_price_wins`, `price_outcome_keeps_the_cli_figure_beside_the_computed_one`, `a_run_with_no_tokens_keeps_the_cli_figure`, `only_a_priced_claude_provider_has_prices`. Also passes: `store::attempts::tests::reprice_moves_a_claude_rows_cli_figure_aside_and_prices_the_cache`. E2e `tests/e2e/providers.rs:715` `a_priced_claude_provider_records_the_computed_cost_beside_the_clis_own` and `:740` `an_unpriced_claude_provider_keeps_the_clis_figure` both pass. `docs/ECONOMIST.md:344` and `src/config/home.rs:346` document pricing, cache defaults and the requested dated $4/$20 and $2/$10 list prices. | None. |
| 668 — argument-count exceptions and documented structs | done | Landed snapshot retains five reasoned test helpers; production argument lists were gathered/split. Current documented structs include `RunAttempt`/`NewAttempt` (`src/attempt.rs:7`, `:22`), `FinishDeploy` (`src/store/deploys.rs:1`), `InsertDecisionBy` (`src/store/record.rs:1`) and `RunDirective` (`src/job/directive.rs:13`). Five current test-helper exceptions have immediately preceding reasons: `src/agent.rs:1375`, `src/agent/chat.rs:290`, `src/store/attempts.rs:896`, `src/store/stats_tests.rs:5`, `:882`. Two additional production exceptions (`src/queue/initiative.rs:143`, `src/cli/initiatives.rs:146`) were introduced later by `ca11285` for priority defaults/CLI flags; they also carry reasons. Contrary to the inherited audit, these two are not test helpers and the current count cannot be subtracted from 29 to infer how many functions task 668 refactored. The requested source-scanning guard `argument_count_allowances_have_a_reason_on_the_line_above` (`src/argument_policy_tests.rs:39`) and matcher test `argument_count_reason_must_be_nonempty_and_immediately_above` (`:53`) both pass. | None for the functions covered by task 668; the two later, reasoned production allowances are distinguished above. |
| 670 — stable plugin FORGE_BIN | done | `src/binary.rs:7` and `:27` derive a stable absolute launch name from inherited `FORGE_BIN` or argv, retaining symlinks; `:12` additionally redirects release paths through `current`. Plugin startup (`src/plugins.rs:606`) and worker usage (`src/worker.rs:882`) call it. Canonical unit-install paths use the commented deleted-suffix helper (`src/binary.rs:49`, `src/init.rs:161`, `:445`). Unit tests `binary::tests::a_release_path_is_named_through_current` and `removes_only_a_trailing_deleted_suffix` pass. E2e `tests/e2e/plugins.rs:2297` `plugin_forge_bin_survives_worker_binary_replacement` passes for both argv and inherited-symlink launch paths: replace binary, restart plugin, invoke the surviving path. `docs/PLUGINS.md:219` promises the stable path across replacement and restarts. | None. |
| 672 — overlay capability and prerelease handling | done | `src/sandbox.rs:173` defines the minimum; `:177` keeps major/minor/patch/pre; Display at `:185` retains `-pre`; parsing at `:196` preserves the suffix; probing at `:220` returns that type; comparison at `:241` requires no prerelease at exactly 0.10.0, but permits 0.10.1-rc.1. Sandbox detection stores the capability and cache binding is conditional, leaving old bwrap caches unbound rather than writable. `src/doctor/binaries.rs:65` reports the displayed version, warns for unsupported overlays and supplies `install bubblewrap >= 0.10` as the fix; `README.md:24` documents the minimum and cold-cache fallback. Passing unit tests: `sandbox::tests::bwrap_versions_parse_and_gate_overlay`, `a_fake_bwrap_version_is_probed`, `without_overlay_support_caches_are_not_bound`. Passing e2e: `tests/e2e/worker.rs:69` `doctor_warns_on_a_prerelease_bwrap_below_the_overlay_minimum` and `:1137` `a_bwrap_without_overlay_support_still_runs_the_attempt_with_no_overlay_flags`. | None in the supplied, visible requirements; the omitted supervisor text was unavailable. |
| 674 — ssh executor | done | `src/executor.rs:31` reports false isolation/egress/credential guarantees and false kernel-controlled checks for ssh; `:100` rsyncs to remote scratch, runs quoted argv with explicit env, then syncs back including on command failure; `:219` configures the destination and `:269` dispatches it. `src/verify/directive.rs:13` returns unverified with the exact reason at `:28`; the directive/integration entry points use this remote verdict. `src/doctor.rs:1167` probes remote CLIs and `:1198` warns when subscription credentials do not travel. E2e `tests/e2e/executors.rs:162` `ssh_executor_syncs_runs_and_retains_an_unverified_branch` and `:167` `ssh_executor_syncs_back_after_a_failed_remote_command` both pass, covering fake ssh, sync, env, retained branch, unverified state and doctor lines. `docs/EXECUTION.md:40` documents executors and their limits. | None. |
| 675 — outcomes, judgment, measured cost and fixture gate | partial | Outcomes are required/enumerated by `src/workflows/definitions.rs:136`, rejected on operations at `:441`, and extracted from job directive output (`src/job/directive.rs:205`). Run-step judgment is required with the rule quoted (`src/workflows/resolve.rs:251`); build workflows are exempt. Cost share is computed at `src/store/stats.rs:779`, joined into measured stats at `src/view/stats.rs:1085` and rendered at `web/src/workflows.js:37`. Fixture validation exists at `src/job/fixture.rs:24`; job enable calls it (`src/cli/jobs.rs:631`) and the first scheduled run calls it (`src/job.rs:434`). Five passing e2e tests in `tests/e2e/execution.rs`: `a_directive_step_records_the_outcome_it_returned_and_one_off_the_list_fails` (`:80`), `an_outcome_on_an_operation_is_refused_by_the_lint` (`:93`), `a_run_workflow_directive_without_judgment_is_refused_quoting_the_rule` (`:113`), `stats_shows_a_workflows_directive_share_of_cost` (`:139`), `an_effect_workflow_is_not_enabled_or_scheduled_until_a_fixture_passes` (`:196`). Fields are documented in `docs/WORKFLOWS.md:454` and `docs/JOBS.md:323`. However plugin enable only checks catalog membership and writes the enabled flag (`src/cli/deploy.rs:330`–`:338`); it neither associates an effectful workflow nor invokes fixture validation. The fixture e2e exercises job enable and scheduling, not plugin enable. | Complete the explicitly requested plugin-enable side of the fixture gate: identify the run workflows an enabled plugin activates, refuse activation of effectful ones until their fixtures pass, and name the fixture path on refusal. Add an e2e covering plugin enable before and after a passing fixture. Preserve the existing job-enable and scheduler gates. |
| 677 — prompt files, includes and attribution | partial | `src/workflows/library.rs:89` loads prompt files beside actions, expands catalog fragments (`:38`), hashes expanded text and individual fragments (`:73`, `:108`), and reports missing fragments/cycles (`:117`). `src/attempt.rs:136` records the action hash and includes; `src/store/stats.rs:920` and `src/cli/statistics.rs:729` provide prompt-hash grouping. E2e `tests/e2e/workflows.rs:746` `two_versions_of_a_prompt_file_are_two_prompt_hash_groups_in_stats` and `:776` `a_missing_include_and_a_cycle_fail_the_catalog_lint` pass. Unit tests `workflows::library::tests::a_fragment_change_changes_every_including_directives_hash` (`src/workflows/library.rs:199`) and `a_missing_include_and_a_cycle_are_blocking_problems` (`:224`) pass. The untrusted-data sentence is shared through `src/prompts.rs:145` and `src/job/directive_text.rs:15`; `prompts::tests::preamble_orders_untrusted_data_then_permission_then_outcome_then_protected` passes. But the shared built-in preamble fragment is compiled in outside catalog include expansion: e.g. `src/builtins/actions/code.toml:1` has neither prompt nor prompt_file, so `src/workflows/definitions.rs:534` gives it an empty prompt hash and `:537` empty includes. Altering the shared fragment therefore does not change that recorded action hash. The requested fragment-change coverage exists only as a unit test, not the named e2e scenario. | Include the shared untrusted-data fragment in built-in directives' recorded prompt/include identity so changes to that fragment change every including directive's recorded hash. Add the requested e2e that runs multiple including directives, changes a catalog fragment, reruns them and verifies changed hashes and persisted include attribution/stats groups. Retain the existing two-prompt-file-version e2e and include/cycle lint coverage. |
| 678 — host execution for verify_ref scratch operations | done | `src/operation.rs:177` chooses the sibling scratch directory; after archiving verify_ref it invokes `f.allow_egress(dir, cfg, t.trust, Some(&t.provider))` (`:187`). That configures execution for this exact path (`src/ctx.rs:307`); backend ancestor lookup (`src/executor.rs:233`) consequently finds Host. The task's exact reproduction test remains at `tests/e2e/executors.rs:108`: `review_host_repo_operation_on_verify_ref_runs_on_host`. It passes with FORGE_SANDBOX=1, checking both branch and verify_ref operations can see the host canary. | None. |

```json
[
  {"task": 665, "verdict": "done", "missing": ""},
  {"task": 666, "verdict": "done", "missing": ""},
  {"task": 667, "verdict": "done", "missing": ""},
  {"task": 668, "verdict": "done", "missing": ""},
  {"task": 670, "verdict": "done", "missing": ""},
  {"task": 672, "verdict": "done", "missing": ""},
  {"task": 674, "verdict": "done", "missing": ""},
  {"task": 675, "verdict": "partial", "missing": "Complete the plugin-enable side of the effectful run-workflow fixture gate. src/cli/deploy.rs:330 only checks catalog membership and sets the plugin enabled flag, unlike job enable and the scheduler. Identify the run workflows the plugin activates, require passing fixtures before activation, and name the fixture path on refusal. Add an e2e proving plugin enable refuses before a passing fixture and succeeds afterward, preserving the existing job-enable and scheduling gates."},
  {"task": 677, "verdict": "partial", "missing": "Include the shared untrusted-data fragment in built-in directives' recorded prompt/include identity: src/prompts.rs:145 includes it outside catalog expansion, while the built-in code action has no prompt and receives an empty hash/includes at src/workflows/definitions.rs:534. A shared-fragment change therefore does not affect that recorded hash. Add the requested e2e that runs multiple including directives, changes a catalog fragment, reruns them and verifies changed hashes, persisted include attribution and stats groups; current fragment-change coverage at src/workflows/library.rs:199 is only a unit test."},
  {"task": 678, "verdict": "done", "missing": ""}
]
```
