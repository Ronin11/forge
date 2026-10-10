//! Drive one task through its workflow to a terminal state. Every task runs
//! a resolved workflow: an ordered list of actions, each a directive (an
//! LLM step, attempted until verified or the budget is spent, every retry
//! told what failed) or an operation (a deterministic command, one shot).
//! The kernel inserts verify after every directive and push after the last
//! action; those are recorded as operations too, so the trace is complete.
//! Every error is classified: a `Task` fault is this task's problem and it
//! fails; an `Env` fault means the worker itself cannot do its job and must
//! stop without blaming the task. The git fault rule: an operation on a
//! task's own worktree (its clone) is a `Task` fault, since only that
//! task's state can make it fail; cloning, fetching from a remote, and
//! taking the repository lock are `Env` faults, since a dead remote or a
//! full disk stops the worker rather than failing the task.

mod capped;
pub(crate) mod cursor;
pub(crate) use capped::landable_capped;
use capped::{Salvage, check_abort, check_cap, salvage};
use cursor::RunCursor;
mod terminal;
use terminal::finish;
pub(crate) use terminal::{finish_fault, settle_ready_initiatives};
mod op;
pub use op::{Classify, Fault};
pub(crate) use op::{OpRow, Timer, op};
mod needs;
use needs::{Environment, apply_environment, environment_after};
mod worktree;
pub use worktree::TaskEnv;
use worktree::prepare_worktree;
pub use worktree::slug;
mod outcome;
pub(crate) use outcome::resume_done;
use outcome::{End, Run, l0_failure_reason, save_cursor};
mod land;
use land::{TryLand, publish, try_land};
mod step;
use step::{RunDirectiveStep, RunOperationStep, StepFlow, run_directive_step, run_operation_step};

use crate::attempt::{Resume, tests_clone_dir};
use crate::checks::CheckResult;
use crate::ctx::Forge;
use crate::landing::{Integrate, integrate, overlay_refs};
use crate::operation::run_operation;
use crate::prompts::early_feedback;
use crate::report::Event;
use crate::store::{AttemptState, Op, Task, TaskState};
use crate::verify::{self, Subject};
use crate::workflows::{self, Contract, Kind};
use crate::{config, git, unix_now};
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// The backend can change under a queued task: leave it blocked with the
/// reason, never failed, before anything runs.
fn block_on_egress(f: &Forge, mut t: Task, reason: String) -> Result<TaskState, Fault> {
    t.state = TaskState::Blocked;
    t.reason = reason;
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    f.store.update_task(&t).env()?;
    f.report.emit(t.id, Event::Note { text: &t.reason });
    Ok(TaskState::Blocked)
}

pub async fn run_task(f: Arc<Forge>, id: i64, wait: bool) -> Result<TaskState, Fault> {
    let mut t = f
        .store
        .task(id)
        .env()?
        .with_context(|| format!("no task {id}"))
        .env()?;
    let repo = PathBuf::from(&t.repo);

    t.state = TaskState::Running;
    t.started_at = Some(unix_now());
    t.worker_pid = Some(std::process::id() as i64);

    let resolved = resolve_workflow(&f, &mut t)?;

    // The repository's remote, from its forge.toml at the base branch.
    let base_cfg = config::load_at(&repo, &repo, &t.base_branch).await.task()?;
    // The trust gate is a claim-time fact about the backend, checked before
    // anything else a bad environment (a missing agent binary, say) could
    // otherwise preempt with an unrelated error.
    if let Err(reason) = f.egress_gate(&base_cfg, t.trust) {
        return block_on_egress(&f, t, reason);
    }
    if base_cfg.execution.backend() != crate::executor::Backend::Ssh || f.sandbox.is_none() {
        crate::unit_path::require_on_path(&crate::agent::agent_bin()).env()?;
    }
    let remote_url = match &base_cfg.push_remote {
        Some(name) => git::remote_url(&repo, name).await,
        None => None,
    };

    let had_worktree = !t.worktree.is_empty();
    let merged_base_retry = prepare_worktree(&f, &mut t, &repo, &base_cfg, &remote_url).await?;
    let resumed = cursor::resume(&f, &mut t, &resolved, had_worktree)?;
    f.store.update_task(&t).env()?;
    let wt = PathBuf::from(&t.worktree);
    // Checks and rules come from the trusted base, never from the branch under test.
    let mut cfg = config::load_at(&repo, &wt, &t.base_sha).await.task()?;
    cfg.protected = f.effective_protected(&t, &cfg.protected);
    if let Err(reason) = f.egress_gate(&cfg, t.trust) {
        return block_on_egress(&f, t, reason);
    }
    f.allow_egress(&wt, &cfg, t.trust, Some(&t.provider));

    announce(&f, &t, &resolved, &wt);

    let task_cap = f.effective_per_task_usd(&t);
    let prior = f.store.attempts(id).env()?;
    let prior_ops = f.store.ops(id).env()?;
    let done_ops: HashSet<i64> = prior_ops
        .iter()
        .filter(|o| !o.kernel && o.ok)
        .map(|o| o.seq)
        .collect();
    let mut attempt_no = prior.len() as i64;
    if let Some(text) = &resumed.note {
        f.report.emit(id, Event::Note { text });
    }
    let mut run = Run {
        hash: resumed.hash,
        idx: resumed.idx,
        seq: 0,
        used: crate::store::seed_used(&prior, &f.store.refunded_attempts(id).env()?),
        owed: resumed.owed,
        done: resume_done(&prior),
    };
    // The cap is checked at claim as well as before every attempt.
    let mut end = match check_abort(&f, &t)? {
        Some(e) => Some(e),
        None => check_cap(&f, &mut t, &resolved, &run.done, task_cap, &wt).await?,
    };
    'run: loop {
        if end.is_some() {
            break 'run;
        }
        while run.idx < resolved.steps.len() {
            if let Some(e) = check_abort(&f, &t)? {
                end = Some(e);
                break;
            }
            let step = &resolved.steps[run.idx];
            let seq = run.step_seq();
            run.seq = seq;
            let flow = match step.action.kind {
                Kind::Operation => {
                    run_operation_step(RunOperationStep {
                        f: &f,
                        t: &mut t,
                        cfg: &cfg,
                        resolved: &resolved,
                        run: &mut run,
                        step,
                        seq,
                        prior_ops: &prior_ops,
                        done_ops: &done_ops,
                        merged_base_retry,
                    })
                    .await?
                }
                Kind::Directive => {
                    run_directive_step(RunDirectiveStep {
                        f: &f,
                        t: &mut t,
                        cfg: &cfg,
                        resolved: &resolved,
                        run: &mut run,
                        step,
                        seq,
                        attempt_no: &mut attempt_no,
                        task_cap,
                        repo: &repo,
                        wt: &wt,
                        remote_url: &remote_url,
                        wait,
                    })
                    .await?
                }
            };
            match flow {
                StepFlow::Next => {
                    run.idx += 1;
                    save_cursor(&f, &t, &run, attempt_no)?;
                }
                StepFlow::Again => save_cursor(&f, &t, &run, attempt_no)?,
                StepFlow::End(e) => {
                    end = Some(e);
                    break;
                }
                StepFlow::Requeue(reason) => {
                    let cursor = run.cursor(&t, attempt_no).to_json();
                    f.store
                        .requeue_at(
                            id,
                            &crate::store::Owner::this_process(),
                            &reason,
                            Some(&cursor),
                        )
                        .env()?;
                    return Ok(TaskState::Queued);
                }
            }
        }
        if end.is_some() {
            break 'run;
        }
        match try_land(TryLand {
            f: &f,
            t: &mut t,
            cfg: &mut cfg,
            resolved: &resolved,
            run: &mut run,
            repo: &repo,
            wt: &wt,
            remote_url: &remote_url,
            base_cfg: &base_cfg,
            attempt_no: &mut attempt_no,
            task_cap,
        })
        .await?
        {
            Some(e) => {
                end = Some(e);
                break 'run;
            }
            None => {
                save_cursor(&f, &t, &run, attempt_no)?;
                continue 'run;
            }
        }
    }
    // A budget cap that left commits has them judged as they stand.
    let mut end = salvage(
        Salvage {
            f: &f,
            t: &mut t,
            cfg: &cfg,
            run: &mut run,
            repo: &repo,
            wt: &wt,
            remote_url: &remote_url,
            base_cfg: &base_cfg,
            attempt_no: &mut attempt_no,
        },
        end.unwrap_or(End::Verified),
    )
    .await?;

    let mut compare: Option<String> = None;
    if end.pushes() {
        run.seq += 1;
        let (c, failed) = publish(&f, &mut t, &wt, &repo, &remote_url, run.seq).await?;
        compare = c;
        if let Some(e) = failed {
            end = e;
        }
    }
    finish(&f, &mut t, &end, compare, &wt).await
}

/// The run's start on the report: where it works, and which workflow it runs.
fn announce(f: &Forge, t: &Task, resolved: &workflows::Resolved, wt: &Path) {
    let id = t.id;
    f.report.emit(
        id,
        Event::TaskStarted {
            worktree: &t.worktree,
            branch: &t.branch,
            base_branch: &t.base_branch,
            base_sha: &t.base_sha,
            model: &t.model,
            max_turns: t.max_turns,
            max_attempts: t.max_attempts,
            timeout_secs: t.timeout_secs,
            sandboxed: f.sandboxed(wt),
        },
    );
    f.report.emit(
        id,
        Event::Note {
            text: &format!(
                "workflow {} {} ({})",
                t.workflow,
                &t.workflow_hash[..t.workflow_hash.len().min(8)],
                resolved
                    .steps
                    .iter()
                    .map(|s| s.action.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" → ")
            ),
        },
    );
}

/// The workflow resolved once, at start: the latest versions of every
/// file now, recorded on the task and read from that record from here
/// on. A resumed task keeps what it resolved.
fn resolve_workflow(f: &Forge, t: &mut Task) -> Result<workflows::Resolved, Fault> {
    let resolved: workflows::Resolved = if t.actions_json.is_empty() {
        // A broken workflow directory would fail every task the same way:
        // that is the worker's environment, not this task's fault.
        let problems = workflows::check(&f.paths.home).env()?;
        if let Some(p) = problems.iter().find(|p| p.blocking) {
            return Err(Fault::Env(anyhow::anyhow!(
                "workflow directory is broken: {} {}",
                p.file,
                p.what
            )));
        }
        let r = workflows::resolve(&f.paths.home, &t.workflow).task()?;
        let wf = workflows::get(&f.paths.home, &t.workflow)
            .env()?
            .with_context(|| format!("unknown workflow {}", t.workflow))
            .task()?;
        t.workflow_hash = wf.hash.clone();
        t.workflow_text = wf.text.clone();
        t.actions_json = serde_json::to_string(&r).env()?;
        r
    } else {
        serde_json::from_str(&t.actions_json)
            .context("the task's recorded resolution does not parse")
            .task()?
    };
    Ok(resolved)
}
