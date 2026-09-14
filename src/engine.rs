//! Drive one task through its workflow to a terminal state. Every task runs
//! a resolved workflow: an ordered list of actions, each a directive (an
//! LLM step, attempted until verified or the budget is spent, every retry
//! told what failed) or an operation (a deterministic command, one shot).
//! The kernel inserts verify after every directive and push after the last
//! action; those are recorded as operations too, so the trace is complete.
//! Every error is classified: a `Task` fault is this task's problem and it
//! fails; an `Env` fault means the worker itself cannot do its job and must
//! stop without blaming the task.

use crate::attempt::{Resume, tests_clone_dir};
use crate::ctx::Forge;
use crate::landing::{Integrate, integrate, overlay_refs};
use crate::operation::run_operation;
use crate::prompts::early_feedback;
use crate::report::Event;
use crate::store::{AttemptState, Op, TaskState};
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

    // Resolve the workflow once, at start: the latest versions of every
    // file now, recorded on the task and read from that record from here
    // on. A resumed task keeps what it resolved.
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

    // The repository's remote, from its forge.toml at the base branch.
    let base_cfg = config::load_at(&repo, &repo, &t.base_branch).await.task()?;
    let remote_url = match &base_cfg.push_remote {
        Some(name) => git::remote_url(&repo, name).await,
        None => None,
    };

    let mut seq: i64 = 0;
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
                match git::fetch_branch(&repo, name, &t.base_branch).await {
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
            &repo,
            &t.base_branch,
            &dir,
            &t.branch,
            base_ref.as_deref(),
            None,
        )
        .await;
        op(
            &f,
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
        t.base_sha = r.task()?;
        // The standing hidden suite as it matches this base; a suite that
        // grows while the task runs is for the landing, not for the coder.
        t.verify_base = git::rev_parse(&repo, "refs/heads/forge-verify")
            .await
            .unwrap_or_default();
        t.worktree = dir.display().to_string();
        // A retry of a task whose branch passed the checks starts from
        // that branch, not from scratch: the review's finding or the
        // operator's answer is the only thing left to act on. Three fresh
        // rebuilds of one verified split cost ten dollars before this.
        if let Some(old) = t.retry_of
            && let Some(from) = verified_branch_of(&f, old).await
        {
            match git::fetch_ref(&dir, &from.source, &from.branch).await {
                Ok(()) if git::is_ancestor(&dir, &t.base_sha, "FETCH_HEAD").await => {
                    let tip = git::rev_parse(&dir, "FETCH_HEAD").await.unwrap_or_default();
                    git::reset_hard(&dir, "FETCH_HEAD").await.task()?;
                    f.report.emit(
                        id,
                        Event::Note {
                            text: &format!(
                                "start    from task {old}'s verified branch {} @ {}",
                                from.branch,
                                &tip[..tip.len().min(8)]
                            ),
                        },
                    );
                }
                Ok(()) => f.report.emit(
                    id,
                    Event::Note {
                        text: &format!(
                            "start    task {old}'s branch does not contain the current base; starting fresh"
                        ),
                    },
                ),
                Err(e) => f.report.emit(
                    id,
                    Event::Note {
                        text: &format!("start    task {old}'s branch could not be fetched ({e:#}); starting fresh"),
                    },
                ),
            }
        }
    }
    f.store.update_task(&t).env()?;
    let wt = PathBuf::from(&t.worktree);
    // Checks and rules come from the trusted base, never from the branch under test.
    let mut cfg = config::load_at(&repo, &wt, &t.base_sha).await.task()?;

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

    let task_cap = t.budget_usd.unwrap_or(f.budget.per_task_usd);
    let prior = f.store.attempts(id).env()?;
    let prior_ops = f.store.ops(id).env()?;
    let mut done_directives: HashSet<i64> = prior
        .iter()
        .filter(|a| a.state == AttemptState::Succeeded)
        .map(|a| a.step_seq)
        .collect();
    let done_ops: HashSet<i64> = prior_ops
        .iter()
        .filter(|o| !o.kernel && o.ok)
        .map(|o| o.seq)
        .collect();
    let mut attempt_no = prior.len() as i64;
    let mut last = AttemptState::Running;
    let mut last_reason = String::new();
    let mut budget_stop: Option<String> = None;
    let mut all_ok = true;

    // Attempts used per directive by this worker (a resumed task's earlier
    // attempts were ended by a worker that died, not by the agent), and
    // feedback owed to a directive by a verifying operation that failed.
    let mut used: HashMap<i64, i64> = HashMap::new();
    let mut owed: HashMap<i64, String> = HashMap::new();
    let mut review_unfinished = false;
    let mut idx = 0usize;
    // The base branch's tip once the task landed on it.
    let mut landed: Option<String> = None;
    // The final push failed: verified work that could not be published is
    // not a success, whatever the last attempt did.
    let mut push_failed = false;
    // Verified alone but could not land within its attempts or budget: the
    // branch is pushed for a human rather than lost.
    let mut stalled = false;
    'run: loop {
        'steps: while idx < resolved.steps.len() {
            let step = &resolved.steps[idx];
            seq = idx as i64 + 1;
            match step.action.kind {
                Kind::Operation => {
                    // A mutating operation counts as done only once the kernel
                    // verified what it committed; a worker that died in between
                    // runs it again, which is harmless: it is deterministic and
                    // a second commit finds nothing to commit. A verifying
                    // operation always runs again after the directive it judges.
                    let verified_here = prior_ops
                        .iter()
                        .any(|o| o.kernel && o.name == "verify" && o.seq == seq && o.ok);
                    if done_ops.contains(&seq)
                        && (!step.action.mutates() || verified_here)
                        && !step.action.verifies
                    {
                        idx += 1;
                        continue;
                    }
                    let (ok, detail) = run_operation(&f, &mut t, &cfg, step, seq).await?;
                    if ok {
                        idx += 1;
                        continue;
                    }
                    if step.action.verifies
                        && let Some(d_idx) = (0..idx)
                            .rev()
                            .find(|&i| resolved.steps[i].action.kind == Kind::Directive)
                    {
                        let d_seq = d_idx as i64 + 1;
                        if *used.get(&d_seq).unwrap_or(&0) < t.max_attempts {
                            let d_name = resolved.steps[d_idx].action.name.clone();
                            f.report.emit(
                                id,
                                Event::Note {
                                    text: &format!(
                                        "verify   {} failed; back to {} for another attempt",
                                        step.action.name, d_name
                                    ),
                                },
                            );
                            owed.insert(d_seq, format!("The `{}` verification failed after your change:\n{}\nFix it, leave the tree clean, and commit.", step.action.name, detail));
                            idx = d_idx;
                            continue;
                        }
                        last = AttemptState::ChecksFailed;
                        last_reason = format!(
                            "operation {} (verifies) failed after {} attempt(s): {}",
                            step.action.name,
                            used.get(&d_seq).unwrap_or(&0),
                            detail.lines().next().unwrap_or("")
                        );
                        all_ok = false;
                        break;
                    }
                    last = AttemptState::ChecksFailed;
                    last_reason = format!(
                        "operation {} failed: {}",
                        step.action.name,
                        detail.lines().next().unwrap_or("")
                    );
                    all_ok = false;
                    break;
                }
                Kind::Directive => {
                    if done_directives.contains(&seq) && !owed.contains_key(&seq) {
                        f.report.emit(
                            id,
                            Event::Note {
                                text: &format!(
                                    "step     {} already verified; resuming",
                                    step.action.name
                                ),
                            },
                        );
                        idx += 1;
                        continue;
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
                    let mut feedback: Option<String> = owed.remove(&seq);
                    let mut resume: Option<Resume> = None;
                    let mut step_ok = false;
                    // The last attempt ran out of turns after committing, tree
                    // clean, no result: the checks can still judge the code.
                    let mut capped_committed = false;
                    while *used.get(&seq).unwrap_or(&0) < t.max_attempts {
                        // A subscription window at its cap: wait for the reset
                        // rather than start an attempt that would be rate limited.
                        while let Some((msg, until)) = crate::worker::window_hold(&f).env()? {
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
                            budget_stop = Some(format!(
                                "task budget reached: ${spent:.4} of ${task_cap:.2} after {attempt_no} attempt(s)"
                            ));
                            all_ok = false;
                            break 'steps;
                        }
                        *used.entry(seq).or_insert(0) += 1;
                        let n = used[&seq];
                        attempt_no += 1;
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
                                text: &format!(
                                    "step     {} ({})",
                                    step.action.name,
                                    step.via.join(" → ")
                                ),
                            },
                        );
                        let timer = Timer::now();
                        let (a, verdict, outcome) = crate::attempt::run_attempt(
                            &f,
                            &ts,
                            &cfg,
                            step,
                            seq,
                            attempt_no,
                            feedback.as_deref(),
                            resume.as_ref(),
                        )
                        .await?;
                        // The kernel's verify, as a row of its own.
                        op(
                            &f,
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
                        // The provider refused the run: not an attempt the agent
                        // spent. The hold at the top of the loop waits for the
                        // window; the same feedback and session go again.
                        if outcome.rate_limited {
                            f.report.emit(id, Event::Note { text: "rate     the provider refused this run; it does not count as an attempt" });
                            *used.entry(seq).or_insert(1) -= 1;
                            continue;
                        }
                        // A check that failed only inside the verification namespace
                        // is the test author's failure, not the coder's: the coder
                        // cannot see those files. Back to the tests step, within its
                        // attempts; this attempt does not count against the coder.
                        if a.state == AttemptState::ChecksFailed
                            && step.action.contract != Contract::Tests
                            && let Some((check, tail)) =
                                verify::tests_fault(&verdict.checks, &cfg.namespace)
                            && let Some(t_idx) = (0..idx)
                                .rev()
                                .find(|&i| resolved.steps[i].action.contract == Contract::Tests)
                        {
                            let t_seq = t_idx as i64 + 1;
                            let t_used = *used.get(&t_seq).unwrap_or(&0);
                            if t_used < t.max_attempts {
                                *used.entry(seq).or_insert(1) -= 1;
                                f.report.emit(id, Event::Note { text: &format!("verify   {check} failed inside {}; back to {} for another attempt", cfg.namespace.join(" "), resolved.steps[t_idx].action.name) });
                                owed.insert(t_seq, format!("The repository's `{check}` check failed on the implementer's tree, and every error is inside your tests:\n{tail}\nThe implementer cannot see or edit those files. Fix your tests so the repository's checks pass with them in place, commit, and describe the interface again."));
                                done_directives.retain(|&d| d < t_seq);
                                // The coder starts over against the corrected tests.
                                git::reset_hard(Path::new(&t.worktree), &t.base_sha)
                                    .await
                                    .task()?;
                                idx = t_idx;
                                continue 'steps;
                            }
                            last_reason = format!(
                                "check {check} failed inside the verification namespace after {t_used} tests attempt(s): {}",
                                tail.lines().next().unwrap_or("")
                            );
                            all_ok = false;
                            break 'steps;
                        }
                        match a.state {
                            AttemptState::Succeeded => {
                                if step.action.contract == Contract::Tests {
                                    let tests_dir = tests_clone_dir(&t.worktree);
                                    git::push_to_repo(
                                        &tests_dir,
                                        &repo,
                                        &format!("verify/{}", t.id),
                                    )
                                    .await
                                    .task()?;
                                    if let Some(url) = &remote_url
                                        && let Err(e) =
                                            git::push(&tests_dir, url, &format!("verify/{}", t.id))
                                                .await
                                    {
                                        f.report.emit(
                                            id,
                                            Event::Note {
                                                text: &format!(
                                                    "tests    push of verify/{} failed: {e:#}",
                                                    t.id
                                                ),
                                            },
                                        );
                                    }
                                    t.interface = verdict
                                        .envelope
                                        .as_ref()
                                        .map(|e| e.summary.clone())
                                        .unwrap_or_default();
                                    f.store.update_task(&t).env()?;
                                }
                                if step.action.contract == Contract::Plan {
                                    // The plan is the product: shown to every later
                                    // directive, verified only to name real paths.
                                    t.plan = verdict
                                        .envelope
                                        .as_ref()
                                        .map(|e| e.summary.clone())
                                        .unwrap_or_default();
                                    f.store.update_task(&t).env()?;
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
                                }
                                step_ok = true;
                                break;
                            }
                            AttemptState::Unverified | AttemptState::NeedsInput => break,
                            AttemptState::ChecksFailed | AttemptState::AgentFailed => {
                                // Out of turns before producing a result: continue the
                                // same session rather than start over blind. Even with
                                // nothing on the tree, the session holds what the agent
                                // located; a fresh attempt would spend its turns finding
                                // it again (33 of the first 214 attempts did exactly
                                // that). A capped attempt that did return a result gets
                                // the ordinary feedback for what its result failed.
                                let capped =
                                    outcome.max_turns_hit || outcome.num_turns >= ts.max_turns;
                                let stopped = outcome.ended_early.is_some();
                                let unfinished = verdict.envelope.is_none();
                                let progress = verdict.commits > 0 || verdict.dirty;
                                capped_committed =
                                    capped && unfinished && verdict.commits > 0 && !verdict.dirty;
                                if (capped || stopped)
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
                                                } else {
                                                    "past the turn cap"
                                                }
                                            ),
                                        },
                                    );
                                    resume = Some(Resume {
                                        session: sid.clone(),
                                        start_sha: a.start_sha.clone(),
                                    });
                                    feedback = Some(if let Some(why) = &outcome.ended_early {
                                        early_feedback(why, &outcome.early_signals)
                                    } else if progress {
                                        "You ran out of turns before finishing. Continue exactly where you left off: finish the work, leave the tree clean, commit, and return the structured result. Its `changes` must list every path you changed since this session began, not only in this continuation; the kernel measures from where you started.".to_string()
                                    } else {
                                        "You ran out of turns before changing anything. You have already read what you need: stop exploring, make the change now, commit as soon as it compiles, and return the structured result. Its `changes` must list every path you changed since this session began.".to_string()
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
                                    });
                                    feedback =
                                        Some(verify::feedback(&verdict, &outcome, ts.max_turns));
                                } else {
                                    resume = None;
                                    feedback =
                                        Some(verify::feedback(&verdict, &outcome, ts.max_turns));
                                }
                            }
                            AttemptState::Running => {
                                unreachable!("attempt returned in running state")
                            }
                        }
                    }
                    if !step_ok {
                        // A coder that ran out of turns after committing a clean
                        // tree left code the checks can judge. If they pass, no
                        // agent vouched for it, so it goes to a human as unverified
                        // rather than being thrown away.
                        if step.action.contract == Contract::Code
                            && last == AttemptState::AgentFailed
                            && capped_committed
                        {
                            let overlay = overlay_refs(&repo, t.id, Some(&t.verify_base)).await;
                            let v = verify::verify_integration(&Subject {
                                task_id: t.id,
                                repo: &repo,
                                worktree: &wt,
                                base_sha: &t.base_sha,
                                start_sha: &t.base_sha,
                                cfg: &cfg,
                                task_checks: &t.checks,
                                paths: &[],
                                allow_protected: t.allow_protected,
                                overlay_refs: &overlay,
                                pending_main: None,
                                sandbox: f.sandbox.as_ref(),
                                report: &f.report,
                                scratch: None,
                            })
                            .await
                            .task()?;
                            if v.state == AttemptState::Succeeded {
                                review_unfinished = true;
                                last = AttemptState::Unverified;
                                last_reason = "ran out of turns after committing; the checks pass but no result was returned, so the branch goes to a human".into();
                            } else {
                                last_reason = format!(
                                    "ran out of turns after committing; the checks fail: {}",
                                    v.reason
                                );
                            }
                            f.report.emit(
                                id,
                                Event::Note {
                                    text: &format!("capped   {last_reason}"),
                                },
                            );
                        }
                        // A reviewer that never reached a verdict is not evidence
                        // of a defect: the branch verified at the code step, so it
                        // goes to a human as unverified instead of failing.
                        if step.action.contract == Contract::Review
                            && last == AttemptState::AgentFailed
                        {
                            review_unfinished = true;
                            last = AttemptState::Unverified;
                            last_reason = format!(
                                "review could not finish ({last_reason}); the branch verified at the code step and goes to human review"
                            );
                        }
                        all_ok = false;
                        break;
                    }
                    idx += 1;
                }
            }
        }
        // Landing: a kernel operation, after the last step and before anything
        // is pushed. The branch verified against the base it started from; it
        // lands only if it also verifies with the base as it is now.
        if !(all_ok && budget_stop.is_none() && t.land) {
            break 'run;
        }
        let (Some(url), Some(remote)) = (&remote_url, &base_cfg.push_remote) else {
            f.report.emit(
                id,
                Event::Note {
                    text: "land     skipped: the repository has no push remote",
                },
            );
            break 'run;
        };
        match integrate(&f, &mut t, url, remote, &mut seq).await? {
            Integrate::Landed(sha) => {
                last_reason = format!("landed {} @ {}", t.base_branch, &sha[..sha.len().min(8)]);
                t.landed_sha = sha.clone();
                landed = Some(sha);
                break 'run;
            }
            Integrate::Rewind { feedback, first } => {
                let Some(c_idx) = (0..resolved.steps.len())
                    .rev()
                    .find(|&i| resolved.steps[i].action.contract == Contract::Code)
                else {
                    last = AttemptState::ChecksFailed;
                    last_reason = format!("landing failed: {first}");
                    all_ok = false;
                    break 'run;
                };
                let c_seq = c_idx as i64 + 1;
                let c_used = *used.get(&c_seq).unwrap_or(&0);
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
                    cfg = config::load_at(&repo, &wt, &t.base_sha).await.task()?;
                    owed.insert(c_seq, feedback);
                    done_directives.retain(|&d| d < c_seq);
                    idx = c_idx;
                    continue 'run;
                }
                last = AttemptState::ChecksFailed;
                last_reason = if spent >= task_cap {
                    format!(
                        "landing failed: {first}; the task budget is spent (${spent:.2} of ${task_cap:.2}), so the verified branch is pushed for a human"
                    )
                } else {
                    format!(
                        "landing failed after {c_used} attempt(s): {first}; the verified branch is pushed for a human"
                    )
                };
                stalled = true;
                all_ok = false;
                break 'run;
            }
            Integrate::Failed(reason) => {
                last = AttemptState::ChecksFailed;
                last_reason = format!("landing failed: {reason}");
                all_ok = false;
                break 'run;
            }
        }
    }

    let mut compare: Option<String> = None;
    let review_demoted =
        last == AttemptState::NeedsInput && last_reason.starts_with("review demoted");
    if all_ok && budget_stop.is_none() {
        last = AttemptState::Succeeded;
    }
    if landed.is_none()
        && ((all_ok && budget_stop.is_none()) || review_demoted || review_unfinished || stalled)
    {
        seq += 1;
        if let Some(url) = &remote_url {
            let timer = Timer::now();
            match git::push(&wt, url, &t.branch).await {
                Ok(()) => {
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
                        &f,
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
                    last_reason = format!("push failed: {e:#}");
                    push_failed = true;
                    f.report.emit(
                        id,
                        Event::PushFailed {
                            error: &format!("{e:#}"),
                        },
                    );
                    op(
                        &f,
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
            match git::push_to_repo(&wt, &repo, &t.branch).await {
                Ok(()) => f.report.emit(
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
    }

    let attempts = f.store.attempts(id).env()?;
    let cost = f.store.task_cost(id).env()?;
    t.state = match last {
        // A budget stop is never a success, whatever the last attempt did.
        _ if budget_stop.is_some() => TaskState::Failed,
        _ if push_failed => TaskState::Failed,
        AttemptState::Succeeded => TaskState::Succeeded,
        AttemptState::Unverified => TaskState::Unverified,
        AttemptState::NeedsInput => TaskState::Blocked,
        _ => TaskState::Failed,
    };
    t.reason = match (budget_stop, last) {
        (Some(b), _) => b,
        (None, _) if push_failed => last_reason,
        (None, AttemptState::Succeeded | AttemptState::NeedsInput) => last_reason,
        (None, AttemptState::Running) => "no attempts ran".into(),
        (None, _) if last_reason.starts_with("operation ") => last_reason,
        (None, _) => format!("{last_reason} (after {} attempt(s))", attempts.len()),
    };
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    f.store.update_task(&t).env()?;

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

/// Where a retry may start from: the parent's branch, when the parent's
/// last real attempt passed the checks (verified, or verified and then
/// demoted by a reviewer) and its clone or its pushed branch still exists.
struct VerifiedBranch {
    source: String,
    branch: String,
}

async fn verified_branch_of(f: &Forge, old: i64) -> Option<VerifiedBranch> {
    let parent = f.store.task(old).ok().flatten()?;
    if parent.branch.is_empty() {
        return None;
    }
    let attempts = f.store.attempts(old).ok()?;
    let last = attempts.iter().rev().find(|a| a.step != "supervisor")?;
    let verified = match last.state {
        AttemptState::Succeeded => true,
        AttemptState::NeedsInput => last.reason.starts_with("review demoted"),
        _ => false,
    };
    if !verified || last.commits == 0 && attempts.iter().all(|a| a.commits == 0) {
        return None;
    }
    if Path::new(&parent.worktree).join(".git").exists() {
        return Some(VerifiedBranch {
            source: parent.worktree.clone(),
            branch: parent.branch.clone(),
        });
    }
    if parent.pushed {
        let repo = Path::new(&parent.repo);
        let cfg = config::load_working(repo).await.ok()?;
        let remote = cfg.push_remote?;
        let url = git::remote_url(repo, &remote).await?;
        return Some(VerifiedBranch {
            source: url,
            branch: parent.branch.clone(),
        });
    }
    None
}
