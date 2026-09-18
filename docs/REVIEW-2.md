# Second architectural review and refactor plan (2026-09-18)

Forge is ten days old. Since the first review on the 14th it has landed
319 commits (100, 100, 43 and 76 a day) and grown from 21,000 to 37,000
kernel lines: projects and initiatives, deploy targets with a smoke
check and a look, jobs, intake and the concierge, the portal, plugins,
and a second and third provider. Nearly all of it landed through Forge
at one directive, table or view per task, and that sizing rule is the
shape of this review: each task copied the nearest working skeleton,
so the tree now holds the same four or five things two, three and four
times, each copy correct on its own.

This review reads the tree cold, one reader for the whole workspace
with no memory of the week, then confirms every concrete claim at its
line. Sizes at the time of reading, with Sunday evening's in brackets:
store.rs 6046 [1814], cli.rs 4870 [2167], view.rs 3951 [793],
workflows.rs 2748, verify.rs 2159, agent.rs 1888, engine.rs 1568 [1150]
with `run_task` at 1082 lines. Tests: 195 e2e in 23 area files and 198
unit, 393 in all, the e2e suite green in 22 seconds; 80 fake agents on
one library. Clippy is clean at `-D warnings`. Most-churned files this
week: cli.rs (67 commits), store.rs (51), view.rs (37).

## 1. Defects and holes confirmed while reading

Fix these first; none needs a refactor.

1. **The portal is outside the boundary test.** tests/boundary.rs:56
   and :88 iterate `["tui", "web", "client"]`; `portal/` was added on
   the 16th and is checked by neither the manifest rule nor the
   source rule nor the documented-verb rule. Nothing today stops the
   portal from adding rusqlite and reading forge.db. The file's own
   comment claims it parses every member manifest.
2. **This week's tables read rows by position.** `task_from_row` and
   `attempt_from_row` read by name against `TASK_COLUMNS` and
   `ATTEMPT_COLUMNS`, checked against `PRAGMA table_info` by one test
   (store.rs:5950). The seven readers added this week
   (`job_from_row` at 4078 through `initiative_from_row` at 4181) read
   `r.get(7)`; the jobs column list is typed out three times, deploy
   targets' three, deploys' twice, and the schema test loops over
   exactly the two old tables. Inserting a column mid-SELECT mis-parses
   silently. Sunday's stage 3 discipline stopped at Sunday's tables.
3. **`Limits.per_day` and `Limits.on_failure` are parsed, validated,
   round-tripped and never read** (workflows.rs:345; `job::run_now`
   consults only `budget_usd` and `input_bytes`). docs/WORKFLOWS.md:145
   documents both as live. A run workflow with `per_day = 1` runs as
   often as it is triggered.
4. **`claim_next_job` is an N+1**: it loads every queued job, then
   issues one UPDATE per row until one sticks (store.rs:4057).
   `claim_next` for tasks is one statement.
5. **Missing indexes on per-render queries**: `tasks(project)`,
   `tasks(initiative)`, `decisions(task_id)`, `deploys(task_id)`,
   `backlog(project)`, `project_repos(project)`. The portal and the
   initiative report walk all of them; the store has 11 indexes and
   none of these.
6. **One hard sleep left in the suite**: tests/e2e/worker.rs:315
   sleeps 500 ms before SIGTERM instead of waiting on the store for
   the claim. Sunday's stage 1 removed the rest.
7. **Jobs are invisible to every client.** report.rs has
   `DeployStarted`/`DeployFinished` and no job events; tui, web and
   portal contain zero occurrences of "job"; forge-client has three
   job methods and a `Deploy` struct (client/src/lib.rs:979) with no
   method that fetches deploys. `forge stats`, `trace`, `tool_stats`
   and `role_stats` read `attempts` and see nothing a job did, so a
   job's directive step has a cost and a model that no measurement
   counts.

## 2. Themes

### 2.1 Two records of one thing: tasks and jobs

A job's directive step resolves its provider through
`ctx::resolve_provider`, launches through `agent::run`, produces the
same outcome, cost, tokens and model as an attempt, and then writes a
`JobStep` instead of an `Attempt`:

| concept        | task side                                        | job side                                  |
|----------------|--------------------------------------------------|-------------------------------------------|
| step record    | `attempts`, `Attempt`/`FinishAttempt`             | `job_steps`, `JobStep`                    |
| verdict        | `attempts.verdict_json` (`Vec<CheckResult>`)      | `jobs.verdict_json` (same type)           |
| agent failure  | `verify::agent_failure` (verify.rs:1232)          | `job::directive_agent_failure` (job.rs:311): same branch order, different strings |
| step env       | `operation::operation_env` (operation.rs:22)      | `job::step_env` (job.rs:58)               |
| scratch tree   | `attempt::scratch_dir`, `operation::op_scratch_dir` | `job::scratch_dir`, `deploy::scratch_dir`: four `git::archive_all` into a temp dir |

docs/JOBS.md chose separate tables on purpose (a job is a run of the
product, not of the factory) and that stays. What should not stay is
the second copy of the machinery underneath: the launcher, the failure
diagnosis, the env and the scratch tree are the same for both and
should be one.

### 2.2 Copied skeletons, again

`agent::Launch { … }` is built at five sites (attempt.rs:397,
job.rs:211, assess.rs:107, deploy_look.rs:134, supervisor.rs:448), each
resolving a provider, choosing a log path, a schema and a timeout, and
each diagnosing failure its own way. operation.rs holds
`resolve_deploy_method`, `resolve_deploy_smoke`, `resolve_provision`
and their three `run_*` twins: each loads the actions catalog, gets one
by name, checks for a run command, builds `FORGE_ARG_<K>` from a map and
calls `checks::run_one`; three `expect("checked by resolve_…")` depend
on the pairing holding by convention.

### 2.3 cli.rs as a second kernel

`intake_accept` (cli.rs:1785, 120 lines) creates the project, emits
`ProjectCreated`, dedupes the backlog and synthesises a `draft` deploy
target: the whole interview-to-project transition lives in the CLI, so
the portal and the supervisor cannot do it. `integrate` (cli.rs:4184,
150 lines) clones, resolves remotes, merges and re-verifies inline
beside landing.rs, which does the same for one task. `initiative_new`
validates providers and workflows with two closures; `project_deploy_add`
and `_set` are fifty lines each. Beside them `withdraw` and
`project_set` are correct three-line delegations, so the discipline
exists and was not applied to the verbs added under time pressure. The
67 commits that touched cli.rs this week are the cost: every verb
shares one file, so every landing conflicts there.

### 2.4 Every row three times

`JobRow`, `JobStepRow`, `JobEffectRow`, `DeployRow`, `DeployTargetRow`
and the initiative rows in view.rs are field-for-field copies of the
store types with hand-written `From` impls, and their doc comments say
so ("mirrors `store::JobStep`"). client/src/lib.rs carries the third
copy. Sunday chose the view layer so that clients see one set of names
regardless of store history; this week's tables were born with the
final names, so their view copies buy nothing. The client copy is
inherent (the boundary forbids the kernel crate) but is only checked
against a fixture for the trace, not for jobs, deploys or the portal.
view.rs also mixes the text renderers (`landed_task_line`,
`first_sentence`, `strip_path_like_tokens`, `truncate_at_word_boundary`)
into the document assembly it was made to keep pure.

### 2.5 `run_task` is a thousand lines again

Sunday's stage 6 left the run at roughly 400 lines with a `Run` cursor
and an `End` value. This week added the retry from a verified branch
with the base merged in (engine.rs:193-313), resume-on-failure, the
dependents' release (1169+), the supervisor escalation and the deploy
hook, all inside the same body. engine.rs is 1568 lines with three
unit tests; `run_task` is reachable only through the e2e suite.

### 2.6 The new modules have no unit tests

job.rs (931 lines), deploy.rs, deploy_look.rs, operation.rs,
landing.rs, assess.rs, concierge.rs, prompts.rs and cli.rs have no
`#[cfg(test)]` block. Each has pure functions that would be cheap to
pin (`bounded`, `string_fields`, `step_env`, `directive_prompt`,
`executor_error_verdict`, `short`, the ask reason text, every prompt
renderer). The e2e suite carries them today, at the cost of one fake
per path.

### 2.7 One store file for twenty-one tables

store.rs is 6046 lines, 123 public functions, 27 migrations, 21 tables
and one `impl Store`. It is not wrong, it is unnavigable, and it is the
second most-conflicted file this week.

## 3. Keep

- **Config layering is in one place.** `ctx::resolve_provider` and the
  `effective_*` functions, all unit-tested; no second implementation.
- **The runner abstraction is clean.** Outside agent.rs the only
  `claude`/`codex` mentions are sandbox.rs binding the real CLIs' state
  directories and config.rs comments; nothing dispatches on a name.
- **Error posture.** `anyhow::Context` is dense; `TaskState::try_from`
  refuses unknown strings (tested). `job::drive` folding every error
  into a verdict string is deliberate per JOBS.md.
- **The suite.** 22 seconds, polling on the store, one fakes library;
  the host dependencies (bwrap, loopback servers, headless Chromium)
  are real and named.
- **The parser matches the docs** for `kind = "run"` except item 3.

## 4. The plan

Ordered by what each stage unlocks. "Forge" marks work precise enough
to hand to Forge as an initiative at one directive, table or view per
task; "hand" marks kernel semantics. Every stage lands green.

**Stage 0. The defects (hand, today). Done 2026-09-18 (a783799): the widened boundary test found the portal invoking `forge answer` and `forge ask`, neither in the client contract; both documented.** The boundary test reads the
workspace members from Cargo.toml and checks every crate that is not
the kernel or repomap, so the next client is covered by existing;
`claim_next_job` as one `UPDATE … RETURNING`; a migration adding the
six indexes; the schema test extended to every table that has a column
list, with lists for the seven new tables added as stage 2 lands them;
`per_day` enforced at `job::start` (count today's jobs for the workflow
in the project, refuse with a plain reason) and `on_failure` marked in
WORKFLOWS.md as parsed and not yet honoured until JOBS.md step 5; the
worker sleep replaced by a wait on the claim; a `deploy_log` method on
forge-client.

**Stage 1. One launcher, one scratch (hand). Done 2026-09-18 (2df9fa2, dda63c8): `directive::{launch, failure, structured}` under five sites, `git::fresh_archive` under three, `operation::resolve_action` under the three pairs; every attempt string unchanged.** `directive.rs` with
`run_directive(f, DirectiveSpec { role, prompt, dir, env, schema,
timeout, budget }) -> DirectiveOutcome` used by attempt, job, assess,
deploy_look and the supervisor; one `agent_failure` with one set of
strings; `Scratch::from_sha(repo, sha)` for the four archive sites;
`operation.rs`'s three resolve/run pairs as one `resolve_action(kind,
name)` returning a struct the runner cannot be called without.

**Stage 2. Store plumbing (Forge, after 0). Done 2026-09-18: nine tasks (423-430, 461), $35.79; one reviewer demotion (the guard test's hand-written file list) answered by the supervisor; store.rs 6046 lines → eight files, the largest 1437, with a test holding every file under 1500 and the positional-read guard reading the directory.** One task per table
family: named readers and one column list for jobs/job_steps/
job_effects, deploys/deploy_targets, projects/project_repos/backlog/
initiatives, assessments/portal_tokens/task_refs, each with the schema
test extended; then store.rs split into `store/{tasks,attempts,jobs,
projects,deploys,stats,schema}.rs` with one `impl Store` block per
file and no behaviour change, one file per task.

**Stage 3. cli.rs verbs into modules (Forge, after 1). Done 2026-09-18, five tasks, $11.39 (31bf278, 5fa266b, 10c1bcc, 71d7cf8): `intake_accept`, `integrate`, `initiative_new`'s validation and `project_deploy_add/set` moved out; a `#[cfg(test)]` in cli.rs now measures every `fn` body and fails on any over 80 lines outside a named allowlist (`main`; the renderers `show`, `trace`, `initiative_report`, `log`, `list_workflows`, `stats`, `quality_stats`; `land_task`, which lands a verified branch and reports every outcome) — a function may leave the list, nothing new joins it.** One verb per
task: `intake_accept` into intake.rs as `accept(f, task) -> Project`;
`integrate` onto landing.rs; `initiative_new`'s validation into
queue.rs beside `parse_initiative_file`; `project_deploy_add/set` into
deploy.rs. The rule after this stage: a cli.rs function parses
arguments, calls one kernel function and prints; thirty lines or fewer.

**Stage 4. The run's seams (hand). Done 2026-09-18 (df2d69b): `run_task` 1082 → 167 lines; the seven pieces moved by line range with the labelled breaks turned into a `StepFlow`; every fake's terminal path unchanged.** `retry_start` (workflow resolution,
remote, verified-branch merge), `escalate` (the supervisor rung) and
`finish` (dependents, deploy hook) out of `run_task`, which keeps the
step loop and the `Run` cursor; target 500 lines; each seam gets unit
tests on its inputs.

**Stage 5. Jobs made visible (Forge, after 2). Done 2026-09-18: five tasks, $16.56; job events, by-role statistics counting directive steps with a kind column, jobs in the TUI, the web client and the portal.** `JobStarted`/
`JobFinished` events; `role_stats` and `tool_stats` reading job steps
in the same union as attempts; a jobs list in the TUI and the web
client; the portal's "Running for you" fed by the same row; one view
per task.

**Stage 6. Unit tests for the new modules (Forge). Done 2026-09-18: six tasks, $10.17; every module that had no unit tests has them, no new fakes.** One module per
task, pure functions only, no new fakes: job, deploy, deploy_look,
operation, landing, assess, concierge, prompts.

**Stage 7. Rows once (Forge, after 2). Done 2026-09-18: four tasks, $8.80; the job and deploy mirrors gone with byte-identical JSON proven first, fixtures for job, deploy and portal parsed by the client crate, render.rs.** The view mirrors for jobs,
deploys and deploy targets deleted in favour of `Serialize` on the
store types (names already agree); fixtures `tests/fixtures/{job,
deploy,portal}.json` captured from real output and parsed by the
client types in the e2e; the text renderers out of view.rs into
`render.rs`.

**Stage 8. Docs and names (Forge, hand for the rename). Done 2026-09-18 for the docs: two tasks, $3.65; README's layout with a test that names every module, SYSTEM.md regenerated. The unit and remote rename waits for a quiet queue.** WORKFLOWS.md's
limits section honest; README's layout listing job, deploy, deploy_look,
assess, concierge, plugins, portal; SYSTEM.md regenerated by the graph
directive after stages 1-4 settle the modules; the unit `forge2-worker`
and the bare remote `forge2.git` renamed to match the repository.

## 5. What this buys

Stage 0 closes the holes a reader found in an afternoon. Stages 1 and
2 collapse the machinery that was copied five times into one launcher,
one scratch, one resolver and one row discipline, so the next feature
copies nothing. Stages 3 and 4 return cli.rs and engine.rs to what
Sunday made them: a parser and an explicit run. Stage 5 makes the
product side of Forge count in the same measurements as the factory
side. Stages 6 to 8 leave the tree testable at the unit and honest in
its docs. Nothing changes what Forge does; every stage is checked by
the same 393 tests plus the ones stage 6 adds.

## 6. Outcome (2026-09-18, evening)

The whole plan landed in one day, like the first one. Stages 0, 1 and 4
by hand in six commits; stages 2, 3, 5, 6, 7 and 8 through Forge as
thirty-one tasks in six initiatives, every one landed by Forge, for
$86.36 of agent time. The supervisor ruled twice, once on a reviewer's
demotion that was right (a guard test with a hand-written file list,
the same shape as the boundary hole stage 0 closed) and whose fix for
the shape rather than the instance was filed as one more task.

Two things the review did not predict. The widened boundary test found
the portal invoking two verbs the client contract never documented, on
its first run. And the day's landings hit a flake: five of forty-nine
test checks ran to the 900 second wall, twice failing a landing. The
cause was in the worker, not the tests: its SIGINT and SIGTERM listeners
were re-created inside every select iteration, and a signal arriving in
the gap between one and the next was lost, a gap that only opens under
load. Fixed by hand (c93cf6c) and confirmed at 24 of 24 rounds under the
same load; a Forge task (462) added the drain for a plugin replaced in
the same tick as a stop and a guard that kills every test-spawned worker
on drop.

The tree: 41,211 kernel lines in 44 files (up from 37,070 in 33: the
split's own headers and 107 new tests), cli.rs 4753 with its rule,
view.rs 3813 without its mirrors, `run_task` 167 lines, the store in
eight files, one module (render.rs, new and tiny) without a unit test,
500 tests in all from 393, the e2e suite still green in 24 seconds.
Every directive launch, failure reading, scratch checkout and operation
resolver exists once. Jobs count in the same statistics as attempts and
show in every client. 73 commits today.
