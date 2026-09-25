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

/// Workflow state and completed operations needed to execute an operation step.
struct RunOperationStep<'a> {
    f: &'a Forge,
    t: &'a mut Task,
    cfg: &'a config::Config,
    resolved: &'a workflows::Resolved,
    run: &'a mut Run,
    step: &'a workflows::ResolvedStep,
    seq: i64,
    prior_ops: &'a [Op],
    done_ops: &'a HashSet<i64>,
    merged_base_retry: bool,
}

/// Workflow and retry state needed to execute a directive step.
struct RunDirectiveStep<'a> {
    f: &'a Forge,
    t: &'a mut Task,
    cfg: &'a config::Config,
    resolved: &'a workflows::Resolved,
    run: &'a mut Run,
    step: &'a workflows::ResolvedStep,
    seq: i64,
    attempt_no: &'a mut i64,
    task_cap: f64,
    repo: &'a Path,
    wt: &'a Path,
    remote_url: &'a Option<String>,
}

/// Candidate, base configuration, and retry state for a landing attempt.
struct TryLand<'a> {
    f: &'a Forge,
    t: &'a mut Task,
    cfg: &'a mut config::Config,
    resolved: &'a workflows::Resolved,
    run: &'a mut Run,
    repo: &'a Path,
    wt: &'a Path,
    remote_url: &'a Option<String>,
    base_cfg: &'a config::Config,
    attempt_no: &'a mut i64,
    task_cap: f64,
}

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
use std::time::{Duration, Instant};

pub enum Fault {
    Task(anyhow::Error),
    Env(anyhow::Error),
}

impl From<Fault> for anyhow::Error {
    fn from(f: Fault) -> Self {
        match f {
            Fault::Task(e) | Fault::Env(e) => e,
        }
    }
}

pub trait Classify<T> {
    fn task(self) -> Result<T, Fault>;
    fn env(self) -> Result<T, Fault>;
}

impl<T, E: Into<anyhow::Error>> Classify<T> for Result<T, E> {
    fn task(self) -> Result<T, Fault> {
        self.map_err(|e| Fault::Task(e.into()))
    }
    fn env(self) -> Result<T, Fault> {
        self.map_err(|e| Fault::Env(e.into()))
    }
}

/// A branch-safe slug from the first few words of the task text.
pub fn slug(task: &str) -> String {
    let mut out = String::new();
    for word in task.split_whitespace().take(5) {
        let w: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if w.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&w);
    }
    out.chars()
        .take(32)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// When an operation started: unix seconds for the row, an `Instant` for
/// the elapsed time, taken together so they always agree.
pub(crate) struct Timer {
    pub(crate) started_at: i64,
    pub(crate) start: Instant,
}

impl Timer {
    pub(crate) fn now() -> Self {
        Self {
            started_at: unix_now(),
            start: Instant::now(),
        }
    }
}

/// One operation row, kernel or user; `task_id` stays a separate parameter
/// of `op` since it is never part of the row's own identity.
pub(crate) struct OpRow<'a> {
    pub(crate) seq: i64,
    pub(crate) name: &'a str,
    pub(crate) kernel: bool,
    pub(crate) ok: bool,
    pub(crate) exit: Option<i32>,
    pub(crate) detail: &'a str,
    pub(crate) attempt_id: Option<i64>,
    pub(crate) output: &'a str,
}

/// Record one operation row, kernel or user.
pub(crate) fn op(f: &Forge, task_id: i64, timer: &Timer, row: OpRow) -> Result<(), Fault> {
    f.store
        .insert_op(&Op {
            task_id,
            seq: row.seq,
            name: row.name.into(),
            kernel: row.kernel,
            started_at: timer.started_at,
            ms: timer.start.elapsed().as_millis() as i64,
            ok: row.ok,
            exit: row.exit,
            detail: row.detail.into(),
            attempt_id: row.attempt_id,
            output: row.output.into(),
            ..Default::default()
        })
        .env()?;
    f.report.emit(
        task_id,
        Event::Op {
            name: row.name,
            kernel: row.kernel,
            ok: row.ok,
            ms: timer.start.elapsed().as_millis(),
            detail: row.detail,
        },
    );
    Ok(())
}

pub async fn run_task(f: Arc<Forge>, id: i64) -> Result<TaskState, Fault> {
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
    let remote_url = match &base_cfg.push_remote {
        Some(name) => git::remote_url(&repo, name).await,
        None => None,
    };

    let merged_base_retry = prepare_worktree(&f, &mut t, &repo, &base_cfg, &remote_url).await?;
    f.store.update_task(&t).env()?;
    let wt = PathBuf::from(&t.worktree);
    // Checks and rules come from the trusted base, never from the branch under test.
    let mut cfg = config::load_at(&repo, &wt, &t.base_sha).await.task()?;
    cfg.protected = f.effective_protected(&t, &cfg.protected);
    f.allow_egress(&wt, &cfg, t.trust);

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
            sandboxed: f.sandboxed(),
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

    let task_cap = f.effective_per_task_usd(&t);
    let prior = f.store.attempts(id).env()?;
    let prior_ops = f.store.ops(id).env()?;
    let done_ops: HashSet<i64> = prior_ops
        .iter()
        .filter(|o| !o.kernel && o.ok)
        .map(|o| o.seq)
        .collect();
    let mut attempt_no = prior.len() as i64;
    let mut run = Run {
        idx: 0,
        seq: 0,
        used: HashMap::new(),
        owed: HashMap::new(),
        done: prior
            .iter()
            .filter(|a| a.state == AttemptState::Succeeded)
            .map(|a| a.step_seq)
            .collect(),
    };
    let mut end: Option<End> = None;
    'run: loop {
        while run.idx < resolved.steps.len() {
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
                    })
                    .await?
                }
            };
            match flow {
                StepFlow::Next => run.idx += 1,
                StepFlow::Again => {}
                StepFlow::End(e) => {
                    end = Some(e);
                    break;
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
            None => continue 'run,
        }
    }
    let mut end = end.unwrap_or(End::Verified);

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

/// The text an environment need is read from: the failing checks' tails,
/// the attempt's reason and the question it asked.
fn environment_text(a: &crate::store::Attempt, verdict: &verify::Verdict) -> String {
    let mut text = a.reason.clone();
    for c in verdict.checks.iter().filter(|c| !c.ok) {
        text.push('\n');
        text.push_str(&c.tail);
    }
    if let Some(q) = verdict
        .envelope
        .as_ref()
        .and_then(|e| e.needs_input.as_ref())
    {
        text.push('\n');
        text.push_str(&q.question);
    }
    text
}

/// What became of an environment need found in a failure's text.
enum Environment {
    /// Nothing recognized, covered or approved: the failure stands.
    Left,
    /// A grant was applied and recorded: the caller runs again.
    Applied,
    /// The supervisor denied it: the task blocks on this question.
    Ask(String),
}

/// Recognize an environment need in `text`. When the policy covers it,
/// apply it to the task's worktree and record the decision row by `forge`;
/// otherwise a host or cache need goes to the supervisor, whose approval
/// within the ceiling is applied and recorded the same way, by `supervisor`
/// (`env_supervisor`).
async fn apply_environment(
    f: &Forge,
    t: &Task,
    cfg: &config::Config,
    text: &str,
) -> Result<Environment, Fault> {
    use crate::environment::Approval;
    let Some(need) = crate::environment::recognize(text) else {
        return Ok(Environment::Left);
    };
    let (grant, by) = if f.environment.covers(&need).is_some() {
        match f.grant_environment(Path::new(&t.worktree), &need, t.trust) {
            Some(g) => (g, Approval::Policy),
            None => return Ok(Environment::Left),
        }
    } else if crate::env_supervisor::applies(f, t, &need) {
        match crate::env_supervisor::rule(f, t, &cfg.environment_deny, &need)
            .await
            .env()?
        {
            crate::env_supervisor::Ruled::Approved(g, why) => {
                match f.apply_grant(Path::new(&t.worktree), g, t.trust) {
                    Some(g) => (g, Approval::Supervisor(why)),
                    None => return Ok(Environment::Left),
                }
            }
            crate::env_supervisor::Ruled::Denied(why) => {
                let q = crate::env_supervisor::question(&need, &why);
                crate::env_supervisor::block(f, t, &need, &q).env()?;
                return Ok(Environment::Ask(q));
            }
        }
    } else {
        return Ok(Environment::Left);
    };
    crate::environment::record(&f.store, t.id, &t.repo, &need, &grant, &by).env()?;
    f.report.emit(
        t.id,
        Event::Note {
            text: &format!(
                "environment {} {} granted ({}); running again, nothing counted",
                need.kind.as_str(),
                need.target,
                grant.describe()
            ),
        },
    );
    Ok(Environment::Applied)
}

/// The run ends blocked on a question for the operator.
fn blocked_on(reason: String) -> StepFlow {
    StepFlow::End(End::Blocked {
        reason,
        demoted: false,
        to: None,
    })
}

/// What one step of the run decided: move to the next step, go round
/// again from wherever the cursor now points (a rewind), or end the run.
enum StepFlow {
    Next,
    Again,
    End(End),
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

/// The task's worktree: a fresh clone of the base (fetched from the remote
/// when there is one) on a branch named for the task, or, for a retry of a
/// task whose branch passed the checks, that branch with the current base
/// merged in. Returns whether such a merge happened: a textually clean
/// merge that does not build surfaces at the setup step, and that failure
/// belongs to the coder, not to the task, since a fresh clone would never
/// see it. Does nothing for a resumed task that already has a worktree.
async fn prepare_worktree(
    f: &Forge,
    t: &mut Task,
    repo: &Path,
    base_cfg: &config::Config,
    remote_url: &Option<String>,
) -> Result<bool, Fault> {
    let id = t.id;
    let seq: i64 = 0;
    // Set when this retry starts from a verified branch with the current
    // base merged into it: a textually clean merge that does not build
    // surfaces at the setup step, and that failure belongs to the coder,
    // not to the task, since a fresh clone would never see it.
    let mut merged_base_retry = false;
    if t.worktree.is_empty() {
        let base_name = format!("forge/{}-{}", t.id, slug(&t.task));
        t.branch = base_name.clone();
        if let Some(url) = &remote_url {
            for k in 2.. {
                if !git::remote_branch_exists(url, &t.branch).await {
                    break;
                }
                t.branch = format!("{base_name}-{k}");
            }
        }
        let dir = f.paths.worktrees.join(t.id.to_string());
        let timer = Timer::now();
        // The base is the remote's, so a task started after a landing sees it.
        let base_ref = match (&base_cfg.push_remote, &remote_url) {
            (Some(name), Some(url)) if git::remote_branch_exists(url, &t.base_branch).await => {
                match git::fetch_branch(repo, name, &t.base_branch).await {
                    Ok(_) => Some(format!("refs/remotes/{name}/{}", t.base_branch)),
                    Err(e) => {
                        f.report.emit(
                            id,
                            Event::Note {
                                text: &format!(
                                    "fetch    {name}/{} failed ({e:#}); using the local base",
                                    t.base_branch
                                ),
                            },
                        );
                        None
                    }
                }
            }
            _ => None,
        };
        let r = git::clone_task(
            repo,
            &t.base_branch,
            &dir,
            &t.branch,
            base_ref.as_deref(),
            None,
        )
        .await;
        op(
            f,
            id,
            &timer,
            OpRow {
                seq,
                name: "clone",
                kernel: true,
                ok: r.is_ok(),
                exit: None,
                detail: &r
                    .as_ref()
                    .map(|s| s[..8].to_string())
                    .unwrap_or_else(|e| format!("{e:#}")),
                attempt_id: None,
                output: "",
            },
        )?;
        t.base_sha = r.env()?;
        // The standing hidden suite as it matches this base; a suite that
        // grows while the task runs is for the landing, not for the coder.
        t.verify_base = git::rev_parse(repo, "refs/heads/forge-verify")
            .await
            .unwrap_or_default();
        t.worktree = dir.display().to_string();
        // A retry of a task whose branch passed the checks starts from
        // that branch, not from scratch: the review's finding or the
        // operator's answer is the only thing left to act on. Three fresh
        // rebuilds of one verified split cost ten dollars before this.
        if let Some(old) = t.retry_of
            && let Some(from) = verified_branch_of(f, old).await
        {
            match git::fetch_ref(&dir, &from.source, &from.branch).await {
                Ok(()) => {
                    let tip = git::rev_parse(&dir, "FETCH_HEAD").await.unwrap_or_default();
                    let short = &tip[..tip.len().min(8)];
                    git::reset_hard(&dir, "FETCH_HEAD").await.task()?;
                    if git::is_ancestor(&dir, &t.base_sha, "HEAD").await {
                        f.report.emit(id, Event::Note { text: &format!("start    from task {old}'s verified branch {} @ {short}", from.branch) });
                    } else {
                        // Main moved while the branch was verified: merge the
                        // current base into it, as the integrator would at
                        // landing, rather than throw the verified work away.
                        // Only a conflict sends the retry back to scratch.
                        let msg = format!("Merge the current base into {}", from.branch);
                        match git::merge(&dir, &t.base_sha, &msg).await {
                            Ok(git::Merge::Merged(_)) | Ok(git::Merge::UpToDate) => {
                                merged_base_retry = true;
                                f.report.emit(id, Event::Note { text: &format!("start    from task {old}'s verified branch {} @ {short}, with the current base merged in", from.branch) });
                            }
                            Ok(git::Merge::Conflict(files)) => {
                                git::reset_hard(&dir, &t.base_sha).await.task()?;
                                f.report.emit(id, Event::Note { text: &format!("start    task {old}'s branch conflicts with the current base in {}; starting fresh", files.join(", ")) });
                            }
                            Err(e) => {
                                git::reset_hard(&dir, &t.base_sha).await.task()?;
                                f.report.emit(id, Event::Note { text: &format!("start    could not merge the current base into task {old}'s branch ({e:#}); starting fresh") });
                            }
                        }
                    }
                }
                Err(e) => f.report.emit(
                    id,
                    Event::Note {
                        text: &format!("start    task {old}'s branch could not be fetched ({e:#}); starting fresh"),
                    },
                ),
            }
        }
    }
    Ok(merged_base_retry)
}

/// One operation step of the run: skipped when an earlier worker already
/// ran and verified it, run otherwise. A verifying operation that fails
/// sends the run back to the directive it judges, within its attempts;
/// `setup` failing after a merged-base retry hands the coder the error
/// instead of failing the task; any other failure ends the run.
async fn run_operation_step(args: RunOperationStep<'_>) -> Result<StepFlow, Fault> {
    let RunOperationStep {
        f,
        t,
        cfg,
        resolved,
        run,
        step,
        seq,
        prior_ops,
        done_ops,
        merged_base_retry,
    } = args;
    // A mutating operation counts as done only once the kernel
    // verified what it committed; a worker that died in between
    // runs it again, which is harmless: it is deterministic and
    // a second commit finds nothing to commit. A verifying
    // operation always runs again after the directive it judges.
    let verified_here = prior_ops
        .iter()
        .any(|o| o.kernel && o.name == "verify" && o.seq == seq && o.ok);
    if done_ops.contains(&seq) && (!step.action.mutates() || verified_here) && !step.action.verifies
    {
        return Ok(StepFlow::Next);
    }
    let (mut ok, mut detail) = run_operation(f, t, cfg, step, seq).await?;
    // A need the [environment] policy covers is granted and the step runs
    // again, no question and nothing counted; each grant applies once.
    while !ok {
        match apply_environment(f, t, cfg, &detail).await? {
            Environment::Applied => (ok, detail) = run_operation(f, t, cfg, step, seq).await?,
            Environment::Ask(reason) => return Ok(blocked_on(reason)),
            Environment::Left => break,
        }
    }
    if ok {
        return Ok(StepFlow::Next);
    }
    // A merge that is textually clean can still be
    // semantically broken; that surfaces here, at setup,
    // before any attempt. It is the merge's problem, not
    // the task's: hand the coder the error on the verified
    // branch instead of failing with nothing to show for it.
    if merged_base_retry
        && step.action.name == "setup"
        && let Some(c_idx) = resolved.steps[run.idx..]
            .iter()
            .position(|s| s.action.kind == Kind::Directive && s.action.contract == Contract::Code)
            .map(|i| run.idx + i)
    {
        let c_name = resolved.steps[c_idx].action.name.clone();
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!(
                    "setup    the merged base does not build; {c_name} will see the error"
                ),
            },
        );
        run.owed.insert(
            c_idx as i64 + 1,
            format!(
                "Setup failed after merging the current base into this verified branch:\n{}\nThe merge is textually clean but the result does not build. Fix it, leave the tree clean, and commit.",
                crate::checks::last_lines(&detail, 30)
            ),
        );
        return Ok(StepFlow::Next);
    }
    if step.action.verifies
        && let Some(d_idx) = (0..run.idx)
            .rev()
            .find(|&i| resolved.steps[i].action.kind == Kind::Directive)
    {
        let d_seq = d_idx as i64 + 1;
        if run.used_at(d_seq) < t.max_attempts {
            let d_name = resolved.steps[d_idx].action.name.clone();
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!(
                        "verify   {} failed; back to {} for another attempt",
                        step.action.name, d_name
                    ),
                },
            );
            run.rewind(d_idx, format!("The `{}` verification failed after your change:\n{}\nFix it, leave the tree clean, and commit.", step.action.name, detail));
            return Ok(StepFlow::Again);
        }
        return Ok(StepFlow::End(End::Failed {
            reason: format!(
                "operation {} (verifies) failed after {} attempt(s): {}",
                step.action.name,
                run.used_at(d_seq),
                detail.lines().next().unwrap_or("")
            ),
            counted: false,
            pushes: false,
        }));
    }
    Ok(StepFlow::End(End::Failed {
        // `setup` gets the full tail: a build failure on a
        // fresh clone means the repository or the base is
        // broken, and `forge show` needs more than the
        // first line of a compiler's output to say why.
        reason: if step.action.name == "setup" {
            format!(
                "operation setup failed:\n{}",
                crate::checks::last_lines(&detail, 30)
            )
        } else {
            format!(
                "operation {} failed: {}",
                step.action.name,
                detail.lines().next().unwrap_or("")
            )
        },
        counted: false,
        pushes: false,
    }))
}

/// One directive step of the run: attempts until one verifies or the
/// directive is out of them, with the window hold, the budget, the resume
/// of a capped session, the tests-fault rewind, and what each contract
/// records on success (the interface, the plan, a filed initiative). When
/// the directive stops short, what that means for the task: a capped
/// coder's clean commit goes to a human if the checks pass, a reviewer
/// that never ruled leaves the branch unverified, a question blocks.
async fn run_directive_step(args: RunDirectiveStep<'_>) -> Result<StepFlow, Fault> {
    let RunDirectiveStep {
        f,
        t,
        cfg,
        resolved,
        run,
        step,
        seq,
        attempt_no,
        task_cap,
        repo,
        wt,
        remote_url,
    } = args;
    let id = t.id;
    if run.done.contains(&seq) && !run.owed.contains_key(&seq) {
        f.report.emit(
            id,
            Event::Note {
                text: &format!("step     {} already verified; resuming", step.action.name),
            },
        );
        return Ok(StepFlow::Next);
    }
    // Per-step parameters: the workflow's override, else the action's default, else the task's.
    let mut ts = t.clone();
    if let Some(m) = &step.model {
        ts.model = m.clone();
    }
    if let Some(n) = step.max_turns {
        ts.max_turns = n as i64;
    }
    if let Some(n) = step.timeout_secs {
        ts.timeout_secs = n as i64;
    }
    // The provider this step's role runs under: the task's
    // own flag, else its project's [roles], else the
    // operator's, else "anthropic" (see
    // `ctx::resolve_provider`). Recorded on `ts.provider` so
    // `attempt::run_attempt`'s existing lookup picks it up.
    let role = step.action.contract.as_str();
    let (provider, provider_source) = f.effective_provider_routed(&ts, role).env()?;
    ts.provider = provider.name.clone();
    // The routing record (docs/ECONOMIST.md, "The routing record"): why
    // this role ran where it did, named alongside what it ran on, before
    // the first attempt spends anything — so even a step that never
    // verifies still shows its own routing.
    t.routing.insert(
        role.to_string(),
        crate::store::RoleRouting {
            provider: crate::store::Routed {
                value: ts.provider.clone(),
                source: provider_source.to_string(),
            },
            model: crate::store::Routed {
                value: crate::attempt::attempt_model(
                    &step.action.name,
                    &ts.model,
                    &ts.model,
                    provider,
                ),
                source: crate::attempt::attempt_model_source(
                    step.model.as_deref(),
                    provider,
                    &t.model_source,
                ),
            },
            workflow: crate::store::Routed {
                value: t.workflow.clone(),
                source: t.workflow_source.clone(),
            },
        },
    );
    f.store.update_task(t).env()?;
    let mut feedback: Option<String> = run.owed.remove(&seq);
    let mut resume: Option<Resume> = None;
    let mut step_ok = false;
    // How the directive's last attempt ended, for the step's End.
    let mut last = AttemptState::Running;
    let mut last_reason = String::new();
    // Who a blocking question is addressed to, from the
    // envelope's `needs_input.to`; `None` means the operator.
    let mut last_to: Option<String> = None;
    // The last attempt's own rows, so the reason built after
    // the loop can name the L0 rules that actually failed
    // rather than rely on `last_reason` alone.
    let mut last_checks: Vec<CheckResult> = Vec::new();
    // The last attempt ran out of turns after committing, tree
    // clean, no result: the checks can still judge the code.
    let mut capped_committed = false;
    while run.used_at(seq) < t.max_attempts {
        // A subscription window at its cap: wait for the reset
        // rather than start an attempt that would be rate limited.
        while let Some((msg, until)) = crate::worker::window_hold(f, &ts.provider).env()? {
            f.report.emit(
                id,
                Event::Note {
                    text: &format!("rate     {msg}; waiting"),
                },
            );
            let wait = (until - unix_now()).clamp(1, 3600) as u64;
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
        let spent = f.store.task_cost(id).env()?;
        if spent >= task_cap {
            // A code attempt already verified, and the run stopped
            // before the review that would vouch for it: not a
            // failure, the same as a review that could not finish.
            let code_verified = resolved.steps.iter().enumerate().any(|(i, s)| {
                s.action.contract == Contract::Code && run.done.contains(&(i as i64 + 1))
            });
            return Ok(StepFlow::End(if code_verified {
                End::Unverified(
                    "budget reached after the code step verified; review did not run".to_string(),
                )
            } else {
                End::Budget(format!(
                    "task budget reached: ${spent:.4} of ${task_cap:.2} after {attempt_no} attempt(s)"
                ))
            }));
        }
        *run.used.entry(seq).or_insert(0) += 1;
        let n = run.used[&seq];
        *attempt_no += 1;
        f.report.emit(
            id,
            Event::AttemptStarted {
                n,
                of: t.max_attempts,
            },
        );
        f.report.emit(
            id,
            Event::Note {
                text: &format!("step     {} ({})", step.action.name, step.via.join(" → ")),
            },
        );
        let timer = Timer::now();
        let (a, verdict, outcome) = crate::attempt::run_attempt(crate::attempt::RunAttempt {
            f,
            t: &ts,
            cfg,
            step,
            seq,
            attempt_no: *attempt_no,
            feedback: feedback.as_deref(),
            resume: resume.as_ref(),
        })
        .await?;
        // A deterministic fix ran before this verdict was
        // decided (see `verify::try_known_fix`): its own
        // row, so the trace shows what Forge did without an
        // agent turn before showing whether it worked.
        if let Some(fix) = &verdict.known_fix {
            op(
                f,
                id,
                &timer,
                OpRow {
                    seq,
                    name: "known-fix",
                    kernel: true,
                    ok: fix.ok,
                    exit: None,
                    detail: &match &fix.commit {
                        Some(sha) => format!(
                            "{} fixed as {}: {}",
                            fix.checks.join(", "),
                            &sha[..8],
                            fix.diff_stat
                        ),
                        None => format!("{} left nothing to commit", fix.checks.join(", ")),
                    },
                    attempt_id: Some(a.id),
                    output: "",
                },
            )?;
        }
        // The kernel's verify, as a row of its own.
        op(
            f,
            id,
            &timer,
            OpRow {
                seq,
                name: "verify",
                kernel: true,
                ok: a.state == AttemptState::Succeeded,
                exit: None,
                detail: &a.reason,
                attempt_id: Some(a.id),
                output: "",
            },
        )?;
        last = a.state;
        last_reason = a.reason.clone();
        last_checks = verdict.checks.clone();
        last_to = verdict
            .envelope
            .as_ref()
            .and_then(|e| e.needs_input.as_ref())
            .and_then(|q| crate::envelope::addressee(q.to.as_deref()));
        // The provider refused the run: not an attempt the agent
        // spent. The hold at the top of the loop waits for the
        // window; the same feedback and session go again.
        if outcome.rate_limited {
            f.report.emit(
                id,
                Event::Note {
                    text: "rate     the provider refused this run; it does not count as an attempt",
                },
            );
            run.refund(seq);
            continue;
        }
        // An environment need the policy covers (a host the proxy
        // refused, a host cache) is applied and the attempt runs again;
        // it does not count against the directive. What the table does
        // not cover falls through as it always has.
        if matches!(
            a.state,
            AttemptState::ChecksFailed | AttemptState::AgentFailed | AttemptState::NeedsInput
        ) {
            match apply_environment(f, t, cfg, &environment_text(&a, &verdict)).await? {
                Environment::Applied => {
                    run.refund(seq);
                    continue;
                }
                Environment::Ask(reason) => return Ok(blocked_on(reason)),
                Environment::Left => {}
            }
        }
        // A check that failed only inside the verification namespace
        // is the test author's failure, not the coder's: the coder
        // cannot see those files. Back to the tests step, within its
        // attempts; this attempt does not count against the coder.
        if a.state == AttemptState::ChecksFailed
            && step.action.contract != Contract::Tests
            && let Some((check, tail)) = verify::tests_fault(&verdict.checks, &cfg.namespace)
            && let Some(t_idx) = (0..run.idx)
                .rev()
                .find(|&i| resolved.steps[i].action.contract == Contract::Tests)
        {
            let t_seq = t_idx as i64 + 1;
            let t_used = run.used_at(t_seq);
            if t_used < t.max_attempts {
                run.refund(seq);
                f.report.emit(
                    id,
                    Event::Note {
                        text: &format!(
                            "verify   {check} failed inside {}; back to {} for another attempt",
                            cfg.namespace.join(" "),
                            resolved.steps[t_idx].action.name
                        ),
                    },
                );
                run.rewind(t_idx, format!("The repository's `{check}` check failed on the implementer's tree, and every error is inside your tests:\n{tail}\nThe implementer cannot see or edit those files. Fix your tests so the repository's checks pass with them in place, commit, and describe the interface again."));
                // The coder starts over against the corrected tests.
                git::reset_hard(Path::new(&t.worktree), &t.base_sha)
                    .await
                    .task()?;
                return Ok(StepFlow::Again);
            }
            return Ok(StepFlow::End(End::Failed {
                reason: format!(
                    "check {check} failed inside the verification namespace after {t_used} tests attempt(s): {}",
                    tail.lines().next().unwrap_or("")
                ),
                counted: true,
                pushes: false,
            }));
        }
        match a.state {
            AttemptState::Succeeded => {
                if step.action.contract == Contract::Tests {
                    let tests_dir = tests_clone_dir(&t.worktree);
                    git::push_to_repo(&f.paths.home, repo, &tests_dir, &format!("verify/{}", t.id))
                        .await
                        .task()?;
                    if let Some(url) = &remote_url
                        && let Err(e) = git::push(
                            &f.paths.home,
                            repo,
                            &tests_dir,
                            url,
                            &format!("verify/{}", t.id),
                        )
                        .await
                    {
                        f.report.emit(
                            id,
                            Event::Note {
                                text: &format!("tests    push of verify/{} failed: {e:#}", t.id),
                            },
                        );
                    }
                    t.interface = verdict
                        .envelope
                        .as_ref()
                        .map(|e| e.summary.clone())
                        .unwrap_or_default();
                    f.store.update_task(t).env()?;
                }
                if step.action.contract == Contract::Plan {
                    // The plan is the product: shown to every later
                    // directive, verified only to name real paths.
                    t.plan = verdict
                        .envelope
                        .as_ref()
                        .map(|e| e.summary.clone())
                        .unwrap_or_default();
                    f.store.update_task(t).env()?;
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!(
                                "plan     {} line(s) from {}",
                                t.plan.lines().count(),
                                step.action.name
                            ),
                        },
                    );
                    // file_into_initiative: the plan's items become
                    // sibling tasks in the same initiative instead
                    // of this task running the code step itself.
                    if step.action.file_into_initiative
                        && let Some(iid) = t.initiative
                    {
                        let filed = crate::queue::file_plan(f, t, iid).await.task()?;
                        f.report.emit(
                            id,
                            Event::Note {
                                text: &format!(
                                    "filed    {} task(s) into initiative {iid}: {}",
                                    filed.len(),
                                    filed
                                        .iter()
                                        .map(i64::to_string)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            },
                        );
                        return Ok(StepFlow::End(End::Filed {
                            n: filed.len(),
                            initiative: iid,
                        }));
                    }
                }
                step_ok = true;
                break;
            }
            AttemptState::NeedsInput => {
                // The interview's confirmation turn writes
                // the brief alongside the question that
                // asks the person to confirm it; every
                // other contract's question stops here
                // with nothing recorded as a plan.
                if step.action.name == "interview"
                    && let Some(summary) = verdict
                        .envelope
                        .as_ref()
                        .map(|e| e.summary.trim().to_string())
                    && !summary.is_empty()
                {
                    t.plan = summary;
                    f.store.update_task(t).env()?;
                }
                break;
            }
            AttemptState::Unverified => break,
            AttemptState::ChecksFailed | AttemptState::AgentFailed => {
                // Out of turns before producing a result: continue the
                // same session rather than start over blind. Even with
                // nothing on the tree, the session holds what the agent
                // located; a fresh attempt would spend its turns finding
                // it again (33 of the first 214 attempts did exactly
                // that). A capped attempt that did return a result gets
                // the ordinary feedback for what its result failed.
                let capped = outcome.max_turns_hit || outcome.num_turns >= ts.max_turns;
                let stopped = outcome.ended_early.is_some();
                let unfinished = verdict.envelope.is_none();
                let progress = verdict.commits > 0 || verdict.dirty;
                capped_committed = capped && unfinished && verdict.commits > 0 && !verdict.dirty;
                let over = fresh_arm(t)
                    && std::fs::read_to_string(&a.log_path)
                        .ok()
                        .and_then(|l| crate::handoff::last_context_tokens(&l))
                        .is_some_and(|n| n > crate::handoff::CONTEXT_THRESHOLD_TOKENS);
                if (capped || stopped || over)
                    && unfinished
                    && let Some(sid) = &outcome.session_id
                {
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!(
                                "resume   continuing session {} {}",
                                &sid[..sid.len().min(8)],
                                if stopped {
                                    "after stopping it early"
                                } else if capped {
                                    "past the turn cap"
                                } else {
                                    "past the context threshold"
                                }
                            ),
                        },
                    );
                    resume = Some(Resume {
                        session: sid.clone(),
                        start_sha: a.start_sha.clone(),
                        fresh_from: fresh_arm(t).then(|| PathBuf::from(&a.log_path)),
                    });
                    feedback = Some(if let Some(why) = &outcome.ended_early {
                        early_feedback(why, &outcome.early_signals)
                    } else if progress {
                        "You ran out of turns before finishing. Continue exactly where you left off: finish the work, leave the tree clean, commit, and return the structured result.".to_string()
                    } else {
                        "You ran out of turns before changing anything. You have already read what you need: stop exploring, make the change now, commit as soon as it compiles, and return the structured result.".to_string()
                    });
                } else if t.resume_on_failure
                    && a.state == AttemptState::ChecksFailed
                    && let Some(sid) = &outcome.session_id
                {
                    // The operator asked to keep going in the same
                    // session after a failed attempt, not just a
                    // capped one: same feedback, same CLI session.
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!(
                                "resume   continuing session {} after failed checks",
                                &sid[..sid.len().min(8)]
                            ),
                        },
                    );
                    resume = Some(Resume {
                        session: sid.clone(),
                        start_sha: a.start_sha.clone(),
                        fresh_from: over.then(|| PathBuf::from(&a.log_path)),
                    });
                    feedback = Some(verify::feedback(&verdict, &outcome, ts.max_turns));
                } else {
                    resume = None;
                    feedback = Some(verify::feedback(&verdict, &outcome, ts.max_turns));
                }
            }
            AttemptState::Running => {
                unreachable!("attempt returned in running state")
            }
        }
    }
    if step_ok {
        run.done.insert(seq);
        return Ok(StepFlow::Next);
    }
    // The directive is out of attempts, or stopped: what that
    // means for the task.
    // A coder that ran out of turns after committing a clean
    // tree left code the checks can judge. If they pass, no
    // agent vouched for it, so it goes to a human as unverified
    // rather than being thrown away.
    if step.action.contract == Contract::Code
        && last == AttemptState::AgentFailed
        && capped_committed
    {
        let overlay = overlay_refs(repo, t.id, Some(&t.verify_base)).await;
        let v = verify::verify_integration(&Subject {
            task_id: t.id,
            repo,
            worktree: wt,
            base_sha: &t.base_sha,
            start_sha: &t.base_sha,
            branch: &t.branch,
            cfg,
            task_checks: &t.checks,
            paths: &[],
            allow_protected: t.allow_protected,
            overlay_refs: &overlay,
            pending_main: None,
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
            scratch: None,
            plan_rows: true,
        })
        .await
        .task()?;
        let reason = if v.state == AttemptState::Succeeded {
            "ran out of turns after committing; the checks pass but no result was returned, so the branch goes to a human".to_string()
        } else {
            format!(
                "ran out of turns after committing; the checks fail: {}",
                v.reason
            )
        };
        f.report.emit(
            id,
            Event::Note {
                text: &format!("capped   {reason}"),
            },
        );
        return Ok(StepFlow::End(if v.state == AttemptState::Succeeded {
            End::Unverified(reason)
        } else {
            End::Failed {
                reason,
                counted: true,
                pushes: false,
            }
        }));
    }
    // A reviewer that never reached a verdict is not evidence
    // of a defect: the branch verified at the code step, so it
    // goes to a human as unverified instead of failing.
    if step.action.contract == Contract::Review && last == AttemptState::AgentFailed {
        return Ok(StepFlow::End(End::Unverified(format!(
            "review could not finish ({last_reason}); the branch verified at the code step and goes to human review"
        ))));
    }
    Ok(StepFlow::End(match last {
        AttemptState::NeedsInput => End::Blocked {
            reason: last_reason.clone(),
            demoted: last_reason.starts_with("review demoted"),
            to: last_to.clone(),
        },
        AttemptState::Unverified => End::Unverified(last_reason.clone()),
        _ => End::Failed {
            reason: l0_failure_reason(&last_checks).unwrap_or_else(|| last_reason.clone()),
            counted: true,
            pushes: false,
        },
    }))
}

/// Landing, after the last step and before anything is pushed: the
/// branch verified against the base it started from, and lands only if it
/// also verifies with the base as it is now. `Some(end)` ends the run;
/// `None` means the landing failed and the run was rewound to the code
/// step with the integrator's feedback, the base moved under it and the
/// config reloaded, and the caller goes round again.
async fn try_land(args: TryLand<'_>) -> Result<Option<End>, Fault> {
    let TryLand {
        f,
        t,
        cfg,
        resolved,
        run,
        repo,
        wt,
        remote_url,
        base_cfg,
        attempt_no,
        task_cap,
    } = args;
    let id = t.id;
    // Landing: a kernel operation, after the last step and before anything
    // is pushed. The branch verified against the base it started from; it
    // lands only if it also verifies with the base as it is now.
    if !t.land {
        return Ok(Some(End::Verified));
    }
    // A level that may not land itself ends here, the checks passed: a
    // person lands it (`forge land`, the inbox page), and the on-landing
    // assessment runs then.
    if !f.trust_policy(t.trust).auto_land {
        let reason = format!(
            "trust {}: the checks passed, but tasks at this level do not land themselves; land it with forge land {id} or from the inbox page",
            t.trust.as_str()
        );
        f.report.emit(
            id,
            Event::Note {
                text: &format!("land     skipped: {reason}"),
            },
        );
        return Ok(Some(End::Unverified(reason)));
    }
    let (Some(url), Some(remote)) = (&remote_url, &base_cfg.push_remote) else {
        f.report.emit(
            id,
            Event::Note {
                text: "land     skipped: the repository has no push remote",
            },
        );
        return Ok(Some(End::Verified));
    };
    let mut seq = run.seq;
    let outcome = integrate(f, t, url, remote, &mut seq, attempt_no).await?;
    run.seq = seq;
    match outcome {
        Integrate::Landed(sha) => {
            t.landed_sha = sha.clone();
            t.landed_at = Some(unix_now());
            Ok(Some(End::Landed(sha)))
        }
        Integrate::Rewind {
            feedback,
            first,
            base_sha,
        } => {
            t.base_sha = base_sha;
            f.store.update_task(t).env()?;
            let Some(c_idx) = (0..resolved.steps.len())
                .rev()
                .find(|&i| resolved.steps[i].action.contract == Contract::Code)
            else {
                return Ok(Some(End::Failed {
                    reason: format!("landing failed: {first}"),
                    counted: true,
                    pushes: false,
                }));
            };
            let c_seq = c_idx as i64 + 1;
            let c_used = run.used_at(c_seq);
            let spent = f.store.task_cost(id).env()?;
            if c_used < t.max_attempts && spent < task_cap {
                f.report.emit(
                    id,
                    Event::Note {
                        text: &format!(
                            "land     back to {} for another attempt",
                            resolved.steps[c_idx].action.name
                        ),
                    },
                );
                // The base moved: its checks and rules are the ones that apply now.
                *cfg = config::load_at(repo, wt, &t.base_sha).await.task()?;
                cfg.protected = f.effective_protected(t, &cfg.protected);
                f.allow_egress(wt, cfg, t.trust);
                run.rewind(c_idx, feedback);
                return Ok(None);
            }
            Ok(Some(End::Failed {
                reason: if spent >= task_cap {
                    format!(
                        "landing failed: {first}; the task budget is spent (${spent:.2} of ${task_cap:.2}), so the verified branch is pushed for a human"
                    )
                } else {
                    format!(
                        "landing failed after {c_used} attempt(s): {first}; the verified branch is pushed for a human"
                    )
                },
                counted: true,
                pushes: true,
            }))
        }
        Integrate::Failed(reason) => Ok(Some(End::Failed {
            reason: format!("landing failed: {reason}"),
            counted: true,
            pushes: false,
        })),
    }
}

/// Publish the branch: pushed to the remote when there is one (a failed
/// push is the `End` returned, since verified work that could not be
/// published is not a success, whatever the last attempt did), else kept
/// in the registered repository where a human can merge it. Returns the
/// compare url when the remote has one.
async fn publish(
    f: &Forge,
    t: &mut Task,
    wt: &Path,
    repo: &Path,
    remote_url: &Option<String>,
    seq: i64,
) -> Result<(Option<String>, Option<End>), Fault> {
    let id = t.id;
    let mut compare: Option<String> = None;
    let mut failed: Option<End> = None;
    if let Some(url) = &remote_url {
        let timer = Timer::now();
        match git::push(&f.paths.home, repo, wt, url, &t.branch).await {
            Ok(_) => {
                t.pushed = true;
                compare = git::compare_url(url, &t.base_branch, &t.branch);
                f.report.emit(
                    id,
                    Event::Pushed {
                        remote: url,
                        branch: &t.branch,
                    },
                );
                op(
                    f,
                    id,
                    &timer,
                    OpRow {
                        seq,
                        name: "push",
                        kernel: true,
                        ok: true,
                        exit: None,
                        detail: &t.branch,
                        attempt_id: None,
                        output: "",
                    },
                )?;
            }
            Err(e) => {
                // Verified work that could not be published is not a
                // success, whatever the last attempt did.
                failed = Some(End::Failed {
                    reason: format!("push failed: {e:#}"),
                    counted: false,
                    pushes: false,
                });
                f.report.emit(
                    id,
                    Event::PushFailed {
                        error: &format!("{e:#}"),
                    },
                );
                op(
                    f,
                    id,
                    &timer,
                    OpRow {
                        seq,
                        name: "push",
                        kernel: true,
                        ok: false,
                        exit: None,
                        detail: &format!("{e:#}"),
                        attempt_id: None,
                        output: "",
                    },
                )?;
            }
        }
    } else {
        // No remote: the branch still leaves the worktree, into the
        // registered repository, where `git branch` shows it and a
        // human can merge it.
        match git::push_to_repo(&f.paths.home, repo, wt, &t.branch).await {
            Ok(_) => f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "kept     {} in {} (no remote to push to)",
                        t.branch,
                        repo.display()
                    ),
                },
            ),
            Err(e) => f.report.emit(
                id,
                Event::Note {
                    text: &format!(
                        "kept     could not put {} in the repository: {e:#}",
                        t.branch
                    ),
                },
            ),
        }
        f.report.emit(id, Event::PushSkipped);
    }
    Ok((compare, failed))
}

/// The end of the run on the record: the task's state and reason derived
/// from `end`, its initiative settled if this was its last task, dependents
/// released now that it landed, failed or went unverified, and the
/// `TaskDone` event.
async fn finish(
    f: &Forge,
    t: &mut Task,
    end: &End,
    compare: Option<String>,
    wt: &Path,
) -> Result<TaskState, Fault> {
    let id = t.id;
    let attempts = f.store.attempts(id).env()?;
    let cost = f.store.task_cost(id).env()?;
    t.state = end.task_state();
    t.reason = end.reason(t, attempts.len());
    t.question_to = end.question_to();
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    f.store.update_task(t).env()?;
    if let Some(iid) = t.initiative {
        crate::view::maybe_settle_initiative(f, id, iid).env()?;
    }
    // A dependent waiting on this task, blocked with a stale reason
    // because its after list has since been re-pointed here, is released
    // or given a fresh reason now that this task itself has landed,
    // failed, or gone unverified; a question or a review demotion
    // (TaskState::Blocked) settles nothing for a dependent to react to.
    if matches!(
        t.state,
        TaskState::Succeeded | TaskState::Failed | TaskState::Unverified
    ) {
        for d in f.store.release_dependents_of(id).env()? {
            f.report.emit(
                d,
                Event::Note {
                    text: "unblocked: its dependencies landed or were withdrawn",
                },
            );
        }
    }

    f.report.emit(
        id,
        Event::TaskDone {
            state: t.state.as_str(),
            attempts: attempts.len(),
            cost,
            reason: &t.reason,
            branch: &t.branch,
            pushed: t.pushed,
            compare: compare.as_deref(),
            remove_cmd: &format!("rm -rf {}", wt.display()),
        },
    );
    Ok(t.state)
}

/// The run's cursor over the resolved steps: where it is, what each
/// directive has spent, what a verifying step or a landing owes a
/// directive as feedback, and which directives are already verified.
/// One `rewind` does every piece of bookkeeping a step back needs; the
/// side effects the caller owns (resetting the tree, reloading the
/// config, refunding an attempt) stay at the call site, named.
struct Run {
    idx: usize,
    /// The op sequence number of the current step; landing and push
    /// continue from it.
    seq: i64,
    /// Attempts used per directive by this worker (a resumed task's
    /// earlier attempts were ended by a worker that died, not by the agent).
    used: HashMap<i64, i64>,
    /// Feedback owed to a directive by a verifying operation or a landing
    /// that failed after it.
    owed: HashMap<i64, String>,
    /// Directives already verified, by sequence number.
    done: HashSet<i64>,
}

impl Run {
    fn step_seq(&self) -> i64 {
        self.idx as i64 + 1
    }

    fn used_at(&self, seq: i64) -> i64 {
        *self.used.get(&seq).unwrap_or(&0)
    }

    /// An attempt that does not count against the directive (refused by
    /// the provider, or a failure that was the test author's).
    fn refund(&mut self, seq: i64) {
        *self.used.entry(seq).or_insert(1) -= 1;
    }

    /// Go back to the directive at `to`, owing it `feedback`; everything
    /// verified from there on is unverified again.
    fn rewind(&mut self, to: usize, feedback: String) {
        let to_seq = to as i64 + 1;
        self.owed.insert(to_seq, feedback);
        self.done.retain(|&d| d < to_seq);
        self.idx = to;
    }
}

/// How a run ended. Set exactly once at the point that decides it; the
/// push decision and the task's state and reason derive from it, so
/// they cannot disagree.
#[derive(Debug)]
enum End {
    /// Every step verified; the branch is pushed for a human (no landing
    /// asked for, or no remote to land on).
    Verified,
    /// Landed on the base at this commit.
    Landed(String),
    /// Verified work that no agent vouched for, or a review that never
    /// finished: pushed, and a human decides.
    Unverified(String),
    /// The agent stopped with a question, or a reviewer demoted the
    /// task; a demoted branch is pushed so the human can look. `to` is
    /// who the question is addressed to (`None` means the operator).
    Blocked {
        reason: String,
        demoted: bool,
        to: Option<String>,
    },
    /// The task failed. `counted` appends the attempt count to the
    /// reason; `pushes` keeps a verified branch that could not land.
    Failed {
        reason: String,
        counted: bool,
        pushes: bool,
    },
    /// The task's cost cap was reached before it finished.
    Budget(String),
    /// A plan step with `file_into_initiative` filed its items as
    /// sibling tasks in the task's initiative; nothing changed the tree,
    /// so nothing is pushed.
    Filed { n: usize, initiative: i64 },
}

/// Names the L0 rows the last attempt's verdict failed, the same shape
/// `verify::decide` reports them in ("L0 failed: has-commits"). `None`
/// when nothing at L0 failed, so the caller falls back to the attempt
/// state's own reason (an agent failure or a question carries no rows).
fn l0_failure_reason(checks: &[CheckResult]) -> Option<String> {
    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| c.level == "L0" && !c.ok)
        .map(|c| c.name.as_str())
        .collect();
    (!failed.is_empty()).then(|| format!("L0 failed: {}", failed.join(", ")))
}

impl End {
    fn pushes(&self) -> bool {
        match self {
            End::Verified | End::Unverified(_) => true,
            End::Landed(_) | End::Budget(_) | End::Filed { .. } => false,
            End::Blocked { demoted, .. } => *demoted,
            End::Failed { pushes, .. } => *pushes,
        }
    }

    fn task_state(&self) -> TaskState {
        match self {
            End::Verified | End::Landed(_) | End::Filed { .. } => TaskState::Succeeded,
            End::Unverified(_) => TaskState::Unverified,
            End::Blocked { .. } => TaskState::Blocked,
            End::Failed { .. } | End::Budget(_) => TaskState::Failed,
        }
    }

    /// Who a blocking question is addressed to; `None` for every other
    /// end, and for a blocked one with no addressee (the operator).
    fn question_to(&self) -> Option<String> {
        match self {
            End::Blocked { to, .. } => to.clone(),
            _ => None,
        }
    }

    fn reason(&self, t: &Task, attempts: usize) -> String {
        match self {
            End::Verified => String::new(),
            End::Landed(sha) => {
                format!("landed {} @ {}", t.base_branch, &sha[..sha.len().min(8)])
            }
            End::Filed { n, initiative } => {
                format!("filed {n} task(s) into initiative {initiative}")
            }
            End::Unverified(r) | End::Blocked { reason: r, .. } | End::Budget(r) => r.clone(),
            End::Failed {
                reason, counted, ..
            } => {
                if *counted {
                    format!("{reason} (after {attempts} attempt(s))")
                } else {
                    reason.clone()
                }
            }
        }
    }
}

/// Where a retry may start from: the parent's branch, when the parent's
/// last real attempt passed the checks (verified, or verified and then
/// demoted by a reviewer) and its clone or its pushed branch still exists.
struct VerifiedBranch {
    source: String,
    branch: String,
}

async fn verified_branch_of(f: &Forge, old: i64) -> Option<VerifiedBranch> {
    // Walk up the retry chain: a retry that itself failed (a rebuild
    // that capped, an integrate the coder could not settle) still has a
    // verified ancestor whose branch is the right place to start.
    let mut id = old;
    let parent = loop {
        let parent = f.store.task(id).ok().flatten()?;
        let attempts = f.store.attempts(id).ok()?;
        let verified = !parent.branch.is_empty()
            && attempts
                .iter()
                .rev()
                .find(|a| a.is_agent())
                .is_some_and(|last| {
                    let ok = match last.state {
                        AttemptState::Succeeded => true,
                        // A demotion the operator or the supervisor set
                        // aside, or a question the repository's checks
                        // already ran and passed on: neither settled the
                        // attempt, but both leave a branch worth resuming
                        // from rather than rebuilding.
                        AttemptState::NeedsInput => {
                            last.reason.starts_with("review demoted")
                                || verify::l1_all_passed(
                                    &serde_json::from_str::<Vec<CheckResult>>(&last.verdict_json)
                                        .unwrap_or_default(),
                                )
                        }
                        _ => false,
                    };
                    ok && (last.commits > 0 || attempts.iter().any(|a| a.commits > 0))
                });
        if verified {
            break parent;
        }
        id = parent.retry_of?;
    };
    // The pushed branch first: it is the copy a person can fix by hand,
    // and the local worktree goes stale the moment someone does (task
    // 269 fetched a worktree that predated the fix on the remote).
    if parent.pushed {
        let repo = Path::new(&parent.repo);
        if let Ok(cfg) = config::load_working(repo).await
            && let Some(remote) = cfg.push_remote
            && let Some(url) = git::remote_url(repo, &remote).await
        {
            return Some(VerifiedBranch {
                source: url,
                branch: parent.branch.clone(),
            });
        }
    }
    if Path::new(&parent.worktree).join(".git").exists() {
        return Some(VerifiedBranch {
            source: parent.worktree.clone(),
            branch: parent.branch.clone(),
        });
    }
    None
}

/// Whether the task drew the `fresh` arm of the continuation factor
/// (docs/CONTEXT.md): a continuation starts a new session on a handoff.
fn fresh_arm(t: &Task) -> bool {
    t.explore.get("continuation").map(String::as_str) == Some("fresh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_state_maps_every_end_variant() {
        let cases: Vec<(End, TaskState)> = vec![
            (End::Verified, TaskState::Succeeded),
            (End::Landed("abc123".to_string()), TaskState::Succeeded),
            (End::Unverified("reason".to_string()), TaskState::Unverified),
            (
                End::Blocked {
                    reason: "reason".to_string(),
                    demoted: false,
                    to: None,
                },
                TaskState::Blocked,
            ),
            (
                End::Blocked {
                    reason: "reason".to_string(),
                    demoted: true,
                    to: None,
                },
                TaskState::Blocked,
            ),
            (
                End::Failed {
                    reason: "reason".to_string(),
                    counted: true,
                    pushes: false,
                },
                TaskState::Failed,
            ),
            (
                End::Budget("task budget reached".to_string()),
                TaskState::Failed,
            ),
            (
                // Budget hit after the code step verified but before review
                // completed: not a failure, a human review the same as a
                // review that could not finish.
                End::Unverified(
                    "budget reached after the code step verified; review did not run".to_string(),
                ),
                TaskState::Unverified,
            ),
        ];
        for (end, expected) in cases {
            assert_eq!(end.task_state(), expected, "{end:?} -> {expected:?}");
        }
    }

    fn check(level: &str, name: &str, ok: bool) -> CheckResult {
        CheckResult {
            level: level.into(),
            name: name.into(),
            ok,
            ..Default::default()
        }
    }

    #[test]
    fn l0_failure_reason_names_failing_l0_rows_and_none_otherwise() {
        assert_eq!(l0_failure_reason(&[]), None);
        assert_eq!(
            l0_failure_reason(&[check("L0", "clean-tree", true)]),
            None,
            "an L0 row that passed names nothing"
        );
        assert_eq!(
            l0_failure_reason(&[check("L1", "tests", false)]),
            None,
            "a failing row outside L0 does not count"
        );
        assert_eq!(
            l0_failure_reason(&[
                check("L0", "clean-tree", true),
                check("L0", "has-commits", false)
            ]),
            Some("L0 failed: has-commits".to_string())
        );
    }

    #[test]
    fn reason_maps_every_end_variant() {
        let t = Task::default();
        let cases: Vec<(End, usize, &str)> = vec![
            (End::Verified, 1, ""),
            (
                End::Landed("abc123def".to_string()),
                1,
                "landed  @ abc123de",
            ),
            (End::Unverified("reason".to_string()), 1, "reason"),
            (
                End::Blocked {
                    reason: "needs input: which one?".to_string(),
                    demoted: false,
                    to: None,
                },
                1,
                "needs input: which one?",
            ),
            (
                End::Failed {
                    reason: "operation setup failed: exit 1".to_string(),
                    counted: false,
                    pushes: false,
                },
                3,
                "operation setup failed: exit 1",
            ),
            (
                End::Failed {
                    reason: "some failure".to_string(),
                    counted: true,
                    pushes: false,
                },
                4,
                "some failure (after 4 attempt(s))",
            ),
            (
                // A landing rewind sent the coder back to commit again; it
                // made none, so the checks failed on has-commits with no
                // agent failure to explain it. The reason built after the
                // attempt loop must name the failing rule, never come out
                // empty (task 232's bug).
                End::Failed {
                    reason: l0_failure_reason(&[
                        check("L0", "clean-tree", true),
                        check("L0", "has-commits", false),
                    ])
                    .expect("has-commits failed"),
                    counted: true,
                    pushes: false,
                },
                4,
                "L0 failed: has-commits (after 4 attempt(s))",
            ),
        ];
        for (end, attempts, expected) in cases {
            assert_eq!(end.reason(&t, attempts), expected, "{end:?}");
            assert!(
                !end.reason(&t, attempts).is_empty() || matches!(end, End::Verified),
                "a non-Verified end must never carry an empty reason: {end:?}"
            );
        }
    }
}
