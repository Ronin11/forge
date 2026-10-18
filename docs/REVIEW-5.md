# Fifth architectural review (2026-10-18)

This review is triggered by engineering-weekly crossing 60 thresholds in
one run. The tree is 117,357 kernel lines. Tests: 1241 unit and 614 e2e,
the e2e suite taking 120 seconds. Clippy reports 0 warnings. The five
largest files are src/job.rs (1494), src/store/mod.rs (1491), src/git.rs
(1466), src/store/tasks.rs (1464) and src/agent.rs (1462); all are under
the 3000-line bound, and all sit within about 50 lines of one another.

The 60 thresholds are three functions over 400 lines and 57 modules
without a `#[cfg(test)]` block. Each is confirmed below at its line.

## 1. Defects and findings

No defect in behaviour was found by the measurement; the findings are
size and coverage. Each function was confirmed at its declared line.

1. **`supervise`** (src/supervisor.rs:462,
   `pub async fn supervise(f: &Forge, id: i64) -> Result<Ruled>`) is 470
   lines.
2. **`run_now_inner`** (src/job.rs:732,
   `async fn run_now_inner(args: RunNow<'_>) -> Result<()>`) is 457
   lines, in the largest file of the tree.
3. **`integrate`** (src/landing.rs:283, `pub async fn integrate(`) is 453
   lines.

## 2. Themes

### 2.1 Three functions past 400 lines

The three above are the only ones over the bound. Review 2 stage 4 took
`run_task` from 1082 to 167 lines by moving pieces by line range; the
same treatment applies to these three.

### 2.2 The next five long functions

Under the bound, but the next ones to cross it:

| at | lines | function |
|----|-------|----------|
| src/store/factors.rs:257 | 381 | `factor_stats` |
| src/operation.rs:113 | 297 | `run_operation` |
| src/view/projects.rs:1005 | 286 | `portal_doc` |
| src/cli/task_records.rs:437 | 270 | `show` |
| src/queue.rs:206 | 257 | `enqueue` |

### 2.3 Modules without unit tests

57 modules have no `#[cfg(test)]` block. By area:

- **cli**: audit_cmd, deploy, eval, eval/report, gc, guard,
  initiatives, jobs, projects, project_targets, secret, statistics,
  stats, task_records, tasks, web, workflows_draft, workflows.
- **engine**: engine.rs, capped, land, needs, op, terminal.
- **store**: claim_marks, factors, job_runs, questions, task_write,
  watch.
- **git**: merge_tree, overlay, tree.
- **doctor**: binaries, capacity, guard, login.
- **job**: directive_text, flow, recovery.
- **view**: jobs, mod, tasks.
- **workflows**: edges, lint, and workflows.rs itself.
- **landing**: effects, round.
- **queue**: dependencies, edit.
- **chat**: doc, mod.
- **others**: admission/size, config/measure, load, login_hold,
  verify/recovery.

The e2e suite carries these today; the unit tier does not.

## 3. Keep

- **Every file is under the 3000-line bound**, the largest at 1494.
- **Clippy is clean** at 0 warnings across 117,357 lines.
- **The tiers are balanced**: 1241 unit and 614 e2e, the e2e suite at
  120 seconds.

## 4. The plan

One task per item, one function or module per task, no behaviour change.

1. Split `supervise` (src/supervisor.rs:462) below 400 lines, seams
   moved by line range, existing e2e unchanged.
2. Split `run_now_inner` (src/job.rs:732) below 400 lines, which also
   moves src/job.rs off the top of the size list.
3. Split `integrate` (src/landing.rs:283) below 400 lines.
4. Add a `#[cfg(test)]` block to each module in 2.3, pure functions
   only, no new fakes, one area per task: cli, engine, store, git,
   doctor, job, view, workflows, landing, queue, chat, others.
5. Watch the five functions in 2.2; split `factor_stats` first.

## 5. What this buys

The three oversized functions return under the bound that engineering-
weekly measures, and the modules the e2e suite alone covers get unit
pins. Nothing changes what Forge does; the 1241 unit and 614 e2e tests
check every step.
