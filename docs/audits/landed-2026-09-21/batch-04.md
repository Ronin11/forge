# Landed-work audit — batch 04 (forge, tasks 501–530)

Method: for each task, `git diff <landed>^2 <landed>` (a "Merge main into…"
landing) or `git diff <base> <landed>` (a direct commit) is the task's own
change; the current tree is then checked directly, and the test(s) the
task names are run with `forge-test cargo test --workspace …` (not the
full suite).

## Table

| task | verdict | evidence | what is missing |
|---|---|---|---|
| 501 | done | `src/doctor.rs:539` `check_learning` measures per `(workflow, provider)` via `src/profile.rs:161` `measure_by_provider`, which reads `src/store/stats.rs:615` `workflow_providers`; `forge workflows` prints each provider's line, `src/cli/workflows.rs:485`. Named test `src/doctor.rs:1375` `check_learning_warns_per_provider_not_on_the_average` passes (`test doctor::tests::check_learning_warns_per_provider_not_on_the_average ... ok`). | none |
| 504 | done | `src/job.rs:171` `record_output` keeps a `tail` and writes the full output under the job's input directory for every operation step, setup included (`src/job.rs:880‑897`, `997‑1010`); `src/store/jobs.rs:151` adds `JobStep.tail`; `forge job show` prints it (`src/cli/jobs.rs`, the `exit`/`|`/`output` lines); the human rung's question quotes the failed step's tail, `src/job.rs:1452` `failure_reason`; documented in `docs/CLIENT.md:440` and `docs/JOBS.md:442‑449`. Named e2e `tests/e2e/jobs.rs:3218` `a_failed_operations_stderr_tail_is_on_its_step_and_in_the_question` passes. | none |
| 505 | done | `docs/OPS.md:319‑322` adds exactly the one requested sentence, nothing else changed in the diff. Note: `backup-store.toml`'s script already had a python3 fallback for hosts without `sqlite3` (commits `d6ceeed`, `24c7e06`, both 2026‑09‑21 14:25–14:27, before this doc commit at 14:32) — the sentence overstates the current requirement, but it is exactly the sentence the task asked for and the task said "nothing else". | Follow‑up: the sentence "the receiving host needs `sqlite3` installed for this check" is stale — the check falls back to `python3` when `sqlite3` is absent (`.forge/workflows/actions/backup-store.toml`, the `command -v sqlite3` branch); the doc should say the check needs one of the two. |
| 514 | done | `src/graph.rs` new module: `Node`/`Edge`/`Graph` (`:36‑51`), `repomap_bin` (`:87`), `build` (`:146`); CLI verb `forge graph <repo> --json` wired in `src/cli/stats.rs:111`; built‑in operation `src/builtins/operations/repo-graph.toml`, registered `src/workflows.rs:875`; documented `docs/ACTIONS.md`, `docs/CLIENT.md` (`GraphDoc`); verb added to the fence, `docs/CLIENT.md:517` (`… journal graph workflows …`). Named unit test `src/graph.rs:228` `files_group_under_their_directory_as_a_module_and_edges_pass_through` passes; `tests/boundary.rs` (3 tests) passes. | none |
| 516 | done | `/graph/modules` route and nav tab, `web/src/app.js:34‑117` (route parsing, links); `web/src/graph.js` — `moduleGraph`, `layoutModules` (layered, no library), `nodeCost`/`nodeDemotions`/`nodeTasks` for the overlay, `renderModuleGraph:144` (size by lines, cost colour, demotion badge, `<title>` hover listing tasks); project selector `wireProjectSelector`, `web/src/app.js`; server route `/api/graph/modules` → `graph_modules`, `web/src/main.rs:376`, itself `forge graph --json` (overlay already populated by an earlier task, `src/cli/stats.rs:118` calling `crate::graph::overlay`). Named test `web/tests/graph_render.rs` `a_three_node_two_edge_fixture_renders_three_nodes_and_two_edges` passes; server test `the_graph_modules_page_and_route_run_forge_graph_and_pass_its_overlaid_json_through` passes; `tests/boundary.rs` passes. | none |
| 523 | done | `src/experiment.rs`: `load`/`validate_factor` (floor, `DEFAULT_FLOOR:28`), `draw_level:199` (independent per‑factor draw), `apply_floor:250` (water‑filling, never below floor), `rebalance:386`, `set_factor`/`save` for the held‑factor question path; wired into the worker at `src/queue.rs:400‑428` (drawn when no `--provider` and no project pin — see note); `.forge/workflows/economist-weekly.toml` (`cron = "0 6 * * 1"`, Monday 06:00 UTC) and its `economist-rebalance` operation; CLI `forge economist rebalance [--dry-run] [--days] [--threshold]`, `src/cli/jobs.rs:740`; docs `docs/ECONOMIST.md` "What is built" (`:103`) and `docs/WORKFLOWS.md`. 22 unit tests in `src/experiment.rs` pass (draw respects weights, floor honoured, rebalance arithmetic, large‑move flagging, commit‑message wording); e2e `tests/e2e/economist.rs` (4 tests, including the named `a_queued_task_without_pins_gets_an_experiment_source_on_its_routing`) pass. | none now, but see note: the commit landed with `if args.provider.is_none() && workflow_source == "default"` (`src/queue.rs` at landing), which per a later commit's own message meant "no task was ever assigned" in practice, since real tasks almost always name a workflow. Fixed same day by `068b3d8` ("experiment: draw providers whatever the workflow's source"), not part of this batch. Current tree (post‑fix) is what the table above evidences. |
| 525 | done | `forge workflows put NAME --stdin --message TEXT [--repo PATH]`, `src/cli/workflows.rs:176` `put_workflow`: lints via `workflows::lint`, refuses a name/`declared_name` mismatch and an empty message, writes+commits in the catalog's git (`git::commit_path`) printing the hash, or with `--repo` files a direct task via `queue::enqueue` printing the task id. Client method `Forge::workflow_put`, `client/src/lib.rs:429`. Documented `docs/CLIENT.md:152‑167` beside retry/answer/ask. Named e2e tests `tests/e2e/workflows.rs`: `forge_workflows_put_lands_in_the_catalog_with_a_commit`, `forge_workflows_put_refuses_a_bad_file_and_writes_nothing`, `forge_workflows_put_repo_files_a_task` — all 3 pass. | none |
| 526 | done | `.forge/workflows/author-workflow.toml` (`kind = "run"`, `trigger.on = "manual"`, steps `dump-workflow-catalog` → `draft-workflow` (role `author`, no tools implied by directive contract `plan`) → `lint-workflow-draft`; `[limits] budget_usd = 0.50, per_day = 40, on_failure = "ask:operator"`). `draft-workflow.toml`'s `schema` matches `{name, kind, description, toml, rationale, open_questions}` exactly. Two fixtures under `.forge/fixtures/author-workflow/`: `01-docs-only-build.json` (`kind: "build"`) and `02-save-note-run.json` (`kind: "run"`). Documented `docs/WORKFLOWS.md` "Authoring". `forge job test author-workflow .` run directly: `pass author-workflow/01-docs-only-build`, `pass author-workflow/02-save-note-run`, `2 fixture(s): 2 passed, 0 failed`, exit 0. | none |
| 527 | done | `/workflows` route lists catalog + every project's repo workflows (`workflows_merged`, `web/src/main.rs`) with kind, step cards (`ForgeWorkflows.renderSteps`) and measured profile; `/workflows/<name>` opens the editor (`web/src/app.js:750` `workflowView`): textarea + 400 ms debounce lint through `POST /api/workflows/<name>/lint` (`web/src/app.js:716‑745` `wireEditor`), resolved steps with contract on hover, profile beside it, Save control that posts `{text, message, project}` to `POST /api/workflows/<name>` and reads "file as a task" for a repo workflow (`saveLabel`, `web/src/app.js:753`). Same `INVALIDATES` token map, `workflows: ['task_done','job_finished']` (`web/src/app.js:20`). Named test `web/tests/workflows_render.rs` `a_fixture_with_two_workflows_renders_two_rows_and_a_lint_problem_renders_at_its_line` passes; server test `the_workflows_page_lists_catalog_and_repo_workflows_and_the_editor_lints_and_saves` passes; `tests/boundary.rs` passes. | none |
| 530 | done | `/requests` page: `web/src/requests.js` `renderRequestRows` (questions with text/to/lineage/last‑attempt‑summary, dependency blocks with the dependency's state) and `renderUnverifiedRows` (land control for unverified tasks); write routes `POST /api/answer/<id>` (`web/src/main.rs:976` `answer_route` → `forge answer`), `POST /api/withdraw/<id>` (`:1016` `withdraw_route` → `forge withdraw --reason`), `POST /api/land/<id>` (→ `forge land`), all token‑gated the same as `/api/retry/<id>`. Refresh on events: `requests: ['task_blocked','task_done']`, `web/src/app.js:26`. Documented beside retry, `docs/CLIENT.md:1576‑1581`. Named test `web/tests/requests_render.rs` `a_fixture_with_two_questions_renders_two_answer_boxes` passes; server test `the_inboxs_write_routes_call_forge_answer_withdraw_and_land_with_the_bodys_text` passes. | none |

## Notes

- Task 505's added sentence is technically exactly what the task text
  asked for, but it was already slightly out of date the moment it
  landed (see table); flagged as a follow‑up, not as "missing" for this
  task's own literal ask.
- Task 523's landed change had a defect that made the draw effectively
  inert on real tasks; a fix landed the same day (`068b3d8`, not one of
  501–530) restores the intended behaviour, which is what the current
  tree's tests exercise. Recorded as evidence, verdict left `done`
  because the current tree — which is what "does the behaviour… exist
  now" asks about — matches the text.

```json
[
  {"task": 501, "verdict": "done", "missing": ""},
  {"task": 504, "verdict": "done", "missing": ""},
  {"task": 505, "verdict": "done", "missing": "docs/OPS.md's new sentence says the receiving host needs sqlite3 installed for the integrity check, but backup-store.toml already falls back to python3 when sqlite3 is absent (landed slightly earlier the same day); file a follow-up to reword the sentence to cover both, so an operator doesn't install sqlite3 unnecessarily or think it's the only option."},
  {"task": 514, "verdict": "done", "missing": ""},
  {"task": 516, "verdict": "done", "missing": ""},
  {"task": 523, "verdict": "done", "missing": ""},
  {"task": 525, "verdict": "done", "missing": ""},
  {"task": 526, "verdict": "done", "missing": ""},
  {"task": 527, "verdict": "done", "missing": ""},
  {"task": 530, "verdict": "done", "missing": ""}
]
```
