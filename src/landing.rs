//! Landing: the integrator. After a task's last directive verifies, the
//! base is fetched, merged in if it moved, the merged tree verified with
//! the standing hidden suite, and the branch fast-forwarded onto the base
//! under a per-repository lock; a conflict or a failing check goes back to
//! the coder as a rewind. `forge land` runs the same function by hand.

use crate::audit::{Inputs, Outputs};
use crate::ctx::Forge;
use crate::engine::{Classify, Fault, OpRow, Timer, op};
use crate::report::Event;
use crate::store::{Attempt, AttemptState, FinishAttempt, Task, TaskState};
use crate::verify::{self, Subject, Verdict};
use crate::{checks, config, git, unix_now};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Record the integrator's own check run as an attempts row, the way a
/// directive's does: no agent ran it, so turns, cost and model are all
/// empty, but the rows and tails are the same ones `verify_integration`
/// produced, and `forge show` / `forge trace --json` / the audit read it
/// like any other attempt.
async fn record_verdict(
    f: &Forge,
    t: &Task,
    attempt_no: i64,
    step_seq: i64,
    sha: &str,
    v: &Verdict,
) -> Result<i64, Fault> {
    let inputs = Inputs {
        step: "integrate".to_string(),
        base_sha: sha.to_string(),
        start_sha: sha.to_string(),
        task_checks: t.checks.clone(),
        ..Default::default()
    };
    let mut a = Attempt {
        task_id: t.id,
        attempt_no,
        step: "integrate".to_string(),
        step_seq,
        start_sha: sha.to_string(),
        inputs_json: serde_json::to_string(&inputs).env()?,
        state: AttemptState::Running,
        started_at: unix_now(),
        ..Default::default()
    };
    a.id = f.store.insert_attempt(&a).env()?;
    f.store
        .finish_attempt(&FinishAttempt {
            id: a.id,
            state: v.state,
            reason: v.reason.clone(),
            finished_at: Some(unix_now()),
            agent_exit: None,
            timed_out: false,
            num_turns: 0,
            tool_calls: 0,
            cost_usd: None,
            agent_ms: 0,
            commits: v.commits,
            files_changed: v.files_changed,
            dirty: v.dirty,
            verdict_json: serde_json::to_string(&v.checks).env()?,
            result_text: String::new(),
            envelope_json: String::new(),
            rl_five_hour: None,
            rl_seven_day: None,
            rl_five_hour_resets: None,
            rl_seven_day_resets: None,
            end_sha: sha.to_string(),
            outputs_json: serde_json::to_string(&Outputs::default()).env()?,
            session_id: String::new(),
            first_edit: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            early_signals: "[]".to_string(),
            early_near: "[]".to_string(),
            cli_cost_usd: None,
        })
        .env()?;
    Ok(a.id)
}

/// The `end_sha` of the most recent attempt that ran in the task's own
/// worktree and succeeded: the commit its verify judged, and so the one
/// landing is about to merge and push. The tests contract runs in its own
/// clone, never `t.worktree`, so it is not a candidate. `None` when the
/// task has no such attempt (an operator-driven flow with nothing on
/// record), in which case the guard that reads this has nothing to check
/// against and stays quiet.
fn last_verified_sha(f: &Forge, task_id: i64) -> Result<Option<String>, Fault> {
    Ok(f.store
        .attempts(task_id)
        .env()?
        .into_iter()
        .rev()
        .find(|a| a.step != "tests" && a.state == AttemptState::Succeeded)
        .map(|a| a.end_sha)
        .filter(|s| !s.is_empty()))
}

pub enum Integrate {
    /// On the base branch; its new tip.
    Landed(String),
    /// The coder has to act: a conflict with the moved base, or checks that
    /// fail with the base merged in. The feedback, its first line, and the
    /// base commit the task now measures from: the caller must set the
    /// task's `base_sha` to it and reload the repository config from there.
    Rewind {
        feedback: String,
        first: String,
        base_sha: String,
    },
    /// Nothing the coder can do about it.
    Failed(String),
}

/// A scratch tree that leaves no trace: removed, with its provider state,
/// when the landing that made it ends by any path.
struct TempTree(PathBuf);

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        crate::sandbox::discard_provider_state(&self.0);
    }
}

/// One landing at a time per repository, across every worker process:
/// an advisory lock on a file under FORGE_HOME, held until dropped.
pub(crate) async fn repo_lock(f: &Forge, repo: &Path) -> Result<std::fs::File, Fault> {
    let dir = f.paths.home.join("locks");
    std::fs::create_dir_all(&dir).env()?;
    let name: String = repo
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{name}.lock")))
        .env()?;
    tokio::task::spawn_blocking(move || file.lock().map(|_| file))
        .await
        .map_err(|e| Fault::Env(anyhow::anyhow!(e)))?
        .env()
}

/// The fields `verify_merged_tree` needs to build a `Subject`: identical
/// between `integrate`'s round loop and `integrate_many`'s per-task loop
/// except for which repository, worktree, checks and overlay refs they name.
struct MergeVerifyArgs<'a> {
    task_id: i64,
    repo: &'a Path,
    worktree: &'a Path,
    base_sha: &'a str,
    /// The branch checked out in `worktree`.
    branch: &'a str,
    cfg: &'a config::Config,
    task_checks: &'a [String],
    allow_protected: bool,
    overlay_refs: &'a [String],
}

/// Verify `args.worktree`'s current tree with every hidden suite in
/// `args.overlay_refs` overlaid. The one piece `integrate` (the base merged
/// into one task's branch, re-verified until landing settles) and
/// `integrate_many` (each task's branch merged in turn onto a fresh clone
/// of the base) share: both merge into their worktree first, in their own
/// way, then call this with only the `Subject` fields differing.
async fn verify_merged_tree(f: &Forge, args: MergeVerifyArgs<'_>) -> Result<Verdict, Fault> {
    verify::verify_integration(&Subject {
        task_id: args.task_id,
        repo: args.repo,
        worktree: args.worktree,
        base_sha: args.base_sha,
        start_sha: args.base_sha,
        branch: args.branch,
        cfg: args.cfg,
        task_checks: args.task_checks,
        paths: &[],
        allow_protected: args.allow_protected,
        overlay_refs: args.overlay_refs,
        pending_main: None,
        sandbox: f.sandbox.as_ref(),
        report: &f.report,
        scratch: None,
        plan_rows: true,
    })
    .await
    .task()
}

/// Land the verified branch on the base branch: bring the base in, verify
/// everything with every hidden suite overlaid, push the branch, fast-forward
/// the base, and fold the task's hidden tests into `forge-verify`. Three
/// rows in the trace: `integrate`, `push`, `land`. Does not mutate the
/// task's `base_sha`: the base commit found along the way is carried out in
/// the result (`Rewind`'s `base_sha`), and it is the caller's job to store
/// it on the task and reload the config from it. `attempt_no` is the same
/// running count `run_attempt` draws its own numbers from, so a check run
/// recorded here never collides with the directive attempt that follows it.
pub async fn integrate(
    f: &Forge,
    t: &mut Task,
    url: &str,
    remote: &str,
    seq: &mut i64,
    attempt_no: &mut i64,
) -> Result<Integrate, Fault> {
    let repo = Path::new(&t.repo);
    let home = &f.paths.home;
    // The agent's clone is only ever a fetch source (and target for the
    // placed base). The merge, the re-verification and every push run from
    // a tree cloned from the kernel-owned repository.
    let clone = Path::new(&t.worktree);
    let _lock = repo_lock(f, repo).await?;
    let branch_ref = format!("refs/heads/{}", t.branch);
    let staged = git::stage(home, repo, clone, "HEAD", &branch_ref)
        .await
        .task()?;
    // The verdict names one commit; landing merges and pushes exactly it.
    // Between that verify and this lock, nothing should have moved the
    // branch — but if it did, catch it here rather than fast-forward the
    // base to something no check ever ran on.
    if let Some(verified) = last_verified_sha(f, t.id)? {
        let head_now = staged.clone();
        if head_now != verified {
            let d = format!(
                "the branch moved before landing began: verified {}, branch is now {}",
                short(&verified),
                short(&head_now)
            );
            *seq += 1;
            let timer = Timer::now();
            op(
                f,
                t.id,
                &timer,
                OpRow {
                    seq: *seq,
                    name: "land",
                    kernel: true,
                    ok: false,
                    exit: None,
                    detail: &d,
                    attempt_id: None,
                    output: "",
                },
            )?;
            return Ok(Integrate::Failed(d));
        }
    }
    let tree = TempTree(f.paths.worktrees.join(format!("landing-{}", t.id)));
    git::kernel_tree(home, repo, &tree.0, &t.branch)
        .await
        .task()?;
    let wt = tree.0.as_path();
    let placed = format!("forge/{}", t.base_branch);
    let mut base_sha = t.base_sha.clone();
    for round in 0..3 {
        *seq += 1;
        let timer = Timer::now();
        // The base as the remote has it; a remote that has no base branch
        // yet gets it from this landing, starting from the local one.
        let main_sha = if git::remote_branch_exists(url, &t.base_branch).await {
            match git::fetch_branch(repo, remote, &t.base_branch).await {
                Ok(s) => s,
                Err(e) => {
                    let d = format!("fetch of {remote}/{} failed: {e:#}", t.base_branch);
                    op(
                        f,
                        t.id,
                        &timer,
                        OpRow {
                            seq: *seq,
                            name: "integrate",
                            kernel: true,
                            ok: false,
                            exit: None,
                            detail: &d,
                            attempt_id: None,
                            output: "",
                        },
                    )?;
                    // A remote that cannot be fetched is the worker's
                    // environment, not this branch's: stop rather than fail it.
                    return Err(Fault::Env(anyhow::anyhow!(d)));
                }
            }
        } else {
            git::rev_parse(repo, &format!("refs/heads/{}", t.base_branch))
                .await
                .task()?
        };
        let mut detail = String::new();
        if main_sha != base_sha {
            let base_ref = format!("refs/forge/base/{}", t.base_branch);
            git::stage(home, repo, repo, &main_sha, &base_ref)
                .await
                .task()?;
            git::place_branch(home, repo, wt, &main_sha, &placed)
                .await
                .task()?;
        }
        if main_sha != base_sha && !git::is_ancestor(wt, &main_sha, "HEAD").await {
            let message = format!("Merge {} into {}", t.base_branch, t.branch);
            match git::merge(wt, &main_sha, &message).await.task()? {
                git::Merge::UpToDate => {}
                git::Merge::Merged(m) => {
                    detail = format!(
                        "merged {} @ {} as {}; ",
                        t.base_branch,
                        &main_sha[..8],
                        &m[..8]
                    );
                }
                git::Merge::Conflict(files) => {
                    git::place_branch(home, repo, clone, &main_sha, &placed)
                        .await
                        .task()?;
                    let d = format!(
                        "{} moved to {}; conflicts in {}",
                        t.base_branch,
                        &main_sha[..8],
                        files.join(", ")
                    );
                    op(
                        f,
                        t.id,
                        &timer,
                        OpRow {
                            seq: *seq,
                            name: "integrate",
                            kernel: true,
                            ok: false,
                            exit: None,
                            detail: &d,
                            attempt_id: None,
                            output: "",
                        },
                    )?;
                    f.report.emit(
                        t.id,
                        Event::Note {
                            text: &format!("integrate {d}"),
                        },
                    );
                    let feedback = format!(
                        "The base branch `{base}` has moved since your branch started, and merging it into your branch conflicts in:\n{files}\nThe current `{base}` is in your clone as the local branch `{placed}`. Run `git merge {placed}`, resolve those conflicts, run the checks, and commit the merge. Report the files you resolved as your changes.",
                        base = t.base_branch,
                        files = files.join("\n"),
                    );
                    return Ok(Integrate::Rewind {
                        feedback,
                        first: d,
                        base_sha,
                    });
                }
            }
        }
        // The branch contains the base as it is now: measure from there.
        if base_sha != main_sha && git::is_ancestor(wt, &main_sha, "HEAD").await {
            base_sha = main_sha.clone();
        }
        let cfg_now = config::load_at(repo, wt, &base_sha).await.task()?;
        f.allow_egress(wt, &cfg_now, t.trust);
        let overlay = overlay_refs(repo, t.id, None).await;
        let candidate = git::head(wt).await.task()?;
        let v = verify_merged_tree(
            f,
            MergeVerifyArgs {
                task_id: t.id,
                repo,
                worktree: wt,
                base_sha: &base_sha,
                branch: &t.branch,
                cfg: &cfg_now,
                task_checks: &t.checks,
                allow_protected: t.allow_protected,
                overlay_refs: &overlay,
            },
        )
        .await?;
        if v.state != AttemptState::Succeeded {
            // The coder continues from the merged tree, as it always has.
            if candidate != staged {
                git::adopt_tree(clone, wt).task()?;
            }
            *attempt_no += 1;
            let attempt_id = record_verdict(f, t, *attempt_no, *seq, &base_sha, &v).await?;
            let d = format!("{detail}{}", v.reason);
            op(
                f,
                t.id,
                &timer,
                OpRow {
                    seq: *seq,
                    name: "integrate",
                    kernel: true,
                    ok: false,
                    exit: None,
                    detail: &d,
                    attempt_id: Some(attempt_id),
                    output: "",
                },
            )?;
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!("integrate {d}"),
                },
            );
            let tails: Vec<String> = v
                .checks
                .iter()
                .filter(|c| !c.ok)
                .map(|c| {
                    format!(
                        "- {} {}:\n{}",
                        c.level,
                        c.name,
                        checks::last_lines(&c.tail, 20)
                    )
                })
                .collect();
            let feedback = format!(
                "With the current `{base}` merged into your branch (it is in your clone as `{placed}`, already merged), verification fails:\n{}\nFix it, run the checks, and commit.",
                tails.join("\n"),
                base = t.base_branch,
            );
            return Ok(Integrate::Rewind {
                feedback,
                first: v.reason.clone(),
                base_sha,
            });
        }
        op(
            f,
            t.id,
            &timer,
            OpRow {
                seq: *seq,
                name: "integrate",
                kernel: true,
                ok: true,
                exit: None,
                detail: &format!(
                    "{detail}verified against {} @ {}",
                    t.base_branch,
                    &base_sha[..8]
                ),
                attempt_id: None,
                output: "",
            },
        )?;

        *seq += 1;
        let timer = Timer::now();
        // The tree that was verified must be the tree that is pushed.
        let moved = match git::stage(home, repo, wt, "HEAD", &branch_ref).await {
            Ok(sha) if sha == candidate => None,
            Ok(sha) => Some(format!(
                "the merged tree moved during verification: verified {}, now {}",
                short(&candidate),
                short(&sha)
            )),
            Err(e) => Some(format!("{e:#}")),
        };
        if let Some(d) = moved {
            op(
                f,
                t.id,
                &timer,
                OpRow {
                    seq: *seq,
                    name: "push",
                    kernel: true,
                    ok: false,
                    exit: None,
                    detail: &d,
                    attempt_id: None,
                    output: "",
                },
            )?;
            return Ok(Integrate::Failed(d));
        }
        if let Err(e) = git::push_sha(home, repo, &candidate, url, &t.branch).await {
            let d = format!("push of {} failed: {e:#}", t.branch);
            op(
                f,
                t.id,
                &timer,
                OpRow {
                    seq: *seq,
                    name: "push",
                    kernel: true,
                    ok: false,
                    exit: None,
                    detail: &d,
                    attempt_id: None,
                    output: "",
                },
            )?;
            return Ok(Integrate::Failed(d));
        }
        t.pushed = true;
        f.report.emit(
            t.id,
            Event::Pushed {
                remote: url,
                branch: &t.branch,
            },
        );
        op(
            f,
            t.id,
            &timer,
            OpRow {
                seq: *seq,
                name: "push",
                kernel: true,
                ok: true,
                exit: None,
                detail: &t.branch,
                attempt_id: None,
                output: "",
            },
        )?;

        *seq += 1;
        let timer = Timer::now();
        if let Err(e) = git::push_sha(home, repo, &candidate, url, &t.base_branch).await {
            let d = format!("fast-forward of {} rejected: {e:#}", t.base_branch);
            op(
                f,
                t.id,
                &timer,
                OpRow {
                    seq: *seq,
                    name: "land",
                    kernel: true,
                    ok: false,
                    exit: None,
                    detail: &d,
                    attempt_id: None,
                    output: "",
                },
            )?;
            if round < 2 {
                f.report.emit(
                    t.id,
                    Event::Note {
                        text: &format!(
                            "land     {} moved underneath; integrating again",
                            t.base_branch
                        ),
                    },
                );
                continue;
            }
            return Ok(Integrate::Failed(d));
        }
        let sha = candidate.clone();
        let _ = git::fetch_branch(repo, remote, &t.base_branch).await;
        // The task's hidden tests join the standing suite.
        let own = format!("verify/{}", t.id);
        let mut folded = String::new();
        if git::ref_exists(repo, &format!("refs/heads/{own}")).await
            && !cfg_now.namespace.is_empty()
        {
            let files = git::ls_tree(repo, &own, &cfg_now.namespace).await.task()?;
            let title = t
                .task
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(72)
                .collect::<String>();
            if !files.is_empty()
                && git::graft(
                    repo,
                    &own,
                    &files,
                    "forge-verify",
                    &format!("Task {}: {title}", t.id),
                )
                .await
                .task()?
                .is_some()
            {
                folded = match git::push_ref(
                    home,
                    repo,
                    repo,
                    "refs/heads/forge-verify",
                    url,
                    "forge-verify",
                )
                .await
                {
                    Ok(_) => format!(
                        "; {} hidden test file(s) folded into forge-verify",
                        files.len()
                    ),
                    Err(e) => format!(
                        "; {} hidden test file(s) folded into forge-verify locally (push failed: {e:#})",
                        files.len()
                    ),
                };
            }
        }
        op(
            f,
            t.id,
            &timer,
            OpRow {
                seq: *seq,
                name: "land",
                kernel: true,
                ok: true,
                exit: None,
                detail: &format!("{} @ {}{folded}", t.base_branch, &sha[..8]),
                attempt_id: None,
                output: "",
            },
        )?;
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!("landed   {} @ {}{folded}", t.base_branch, &sha[..8]),
            },
        );
        deploy_on_landing(f, t, &sha).await;
        crate::assess::run_on_landing(f, t, &sha).await;
        return Ok(Integrate::Landed(sha));
    }
    unreachable!("the landing loop returns")
}

/// After landing, run every on-landing deploy target of the task's project
/// on this repository, through the same path `forge deploy` uses
/// (`deploy::run`), tied to this task. A deploy's own failure never
/// changes the task's landed state: `deploy::run` already emits its
/// events and, on a failed check, follows the rollback-and-question path.
async fn deploy_on_landing(f: &Forge, t: &Task, sha: &str) {
    let Some(project) = t.project.clone() else {
        return;
    };
    let Ok(targets) = f.store.deploy_targets(&project) else {
        return;
    };
    for target in targets
        .into_iter()
        .filter(|d| d.on_landing && d.repo == t.repo)
    {
        let _ =
            crate::deploy::run(f, &project, &target.name, Some(sha.to_string()), Some(t.id)).await;
    }
}

/// The refs whose namespace files verify a task: the standing suite and
/// the task's own tests. `pinned` is the standing suite's commit as of the
/// task's base (empty when there was none); `None` means the current tip,
/// which only a tree that already contains the current base may be judged by.
pub async fn overlay_refs(repo: &Path, task_id: i64, pinned: Option<&str>) -> Vec<String> {
    let mut refs = Vec::new();
    match pinned {
        Some("") => {}
        Some(sha) => refs.push(sha.to_string()),
        None => {
            if git::ref_exists(repo, "refs/heads/forge-verify").await {
                refs.push("forge-verify".to_string());
            }
        }
    }
    let own = format!("verify/{task_id}");
    if git::ref_exists(repo, &format!("refs/heads/{own}")).await {
        refs.push(own);
    }
    refs
}

/// One task's outcome in `integrate_many`'s merge order.
pub enum IntegrateStep {
    AlreadyContained {
        task_id: i64,
    },
    Merged {
        task_id: i64,
        branch: String,
        sha: String,
    },
    Verified {
        task_id: i64,
    },
}

impl IntegrateStep {
    fn render(&self) -> String {
        match self {
            Self::AlreadyContained { task_id } => format!("task {task_id:<4} already contained"),
            Self::Merged {
                task_id,
                branch,
                sha,
            } => format!("task {task_id:<4} merged {branch} as {}", short(sha)),
            Self::Verified { task_id } => {
                format!("task {task_id:<4} verified with everything before it")
            }
        }
    }
}

/// How `integrate_many` ended: every named branch merged and left as a
/// branch on the base, or the task where it stopped and why.
pub enum IntegrateOutcome {
    Ready,
    Conflict {
        task_id: i64,
        files: Vec<String>,
        completed: usize,
    },
    VerifyFailed {
        task_id: i64,
        reason: String,
        completed: usize,
    },
}

/// `integrate_many`'s result: the report `forge integrate` prints.
pub struct IntegrateReport {
    base_branch: String,
    base_sha: String,
    repo: PathBuf,
    branch: String,
    dir: PathBuf,
    steps: Vec<IntegrateStep>,
    pub outcome: IntegrateOutcome,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

impl IntegrateReport {
    pub fn render(&self) -> String {
        let mut lines = vec![format!(
            "base     {} @ {}",
            self.base_branch,
            short(&self.base_sha)
        )];
        lines.extend(self.steps.iter().map(IntegrateStep::render));
        match &self.outcome {
            IntegrateOutcome::Ready => {
                let landed = self
                    .steps
                    .iter()
                    .filter(|s| matches!(s, IntegrateStep::Verified { .. }))
                    .count();
                lines.push(format!(
                    "integrated {landed} task(s) as {} in {}\n  git -C {} merge --ff-only {}",
                    self.branch,
                    self.repo.display(),
                    self.repo.display(),
                    self.branch,
                ));
            }
            IntegrateOutcome::Conflict {
                task_id,
                files,
                completed,
            } => {
                lines.push(format!(
                    "task {task_id:<4} CONFLICT in {}",
                    files.join(", ")
                ));
                lines.push(format!(
                    "stopped after {completed} task(s); the scratch clone is at {}",
                    self.dir.display()
                ));
            }
            IntegrateOutcome::VerifyFailed {
                task_id,
                reason,
                completed,
            } => {
                lines.push(format!(
                    "task {task_id:<4} checks FAIL after merging: {reason}"
                ));
                lines.push(format!(
                    "stopped after {completed} task(s); the scratch clone is at {}",
                    self.dir.display()
                ));
            }
        }
        lines.join("\n")
    }
}

/// The integrator's merge-and-verify half, by hand, for a repository that
/// keeps a human at the gate: each named task's branch merged onto the base
/// in order, every check with every hidden suite after each, the result
/// left as a branch. Shares `verify_merged_tree` with `integrate`: the
/// difference is that every task here merges unconditionally (there is no
/// existing branch that might already contain the base) and the `Subject`
/// carries no task-specific checks, since the result is not any one task's
/// branch.
pub async fn integrate_many(f: &Forge, ids: &[i64]) -> Result<IntegrateReport> {
    if ids.is_empty() {
        bail!("name the verified tasks to integrate, in merge order");
    }
    let mut tasks = Vec::new();
    for id in ids {
        let Some(t) = f.store.task(*id)? else {
            bail!("no task {id}");
        };
        if t.state != TaskState::Succeeded && t.state != TaskState::Unverified {
            bail!(
                "task {id} is {}; only a verified task's branch is integrated",
                t.state.as_str()
            );
        }
        tasks.push(t);
    }
    let repo = PathBuf::from(&tasks[0].repo);
    if tasks.iter().any(|t| t.repo != tasks[0].repo) {
        bail!("the tasks are in different repositories");
    }
    let cfg = config::load_working(&repo).await?;
    let stamp = unix_now();
    let branch = format!("forge/integration-{stamp}");
    let dir = f.paths.worktrees.join(format!("integrate-{stamp}"));
    let base_ref = match (
        &cfg.push_remote,
        git::remote_url(&repo, cfg.push_remote.as_deref().unwrap_or("origin")).await,
    ) {
        (Some(name), Some(url)) if git::remote_branch_exists(&url, &cfg.base_branch).await => {
            git::fetch_branch(&repo, name, &cfg.base_branch).await.ok();
            Some(format!("refs/remotes/{name}/{}", cfg.base_branch))
        }
        _ => None,
    };
    let base_sha = git::clone_task(
        &repo,
        &cfg.base_branch,
        &dir,
        &branch,
        base_ref.as_deref(),
        None,
    )
    .await?;
    let remote_url = match &cfg.push_remote {
        Some(name) => git::remote_url(&repo, name).await,
        None => None,
    };
    let mut overlay: Vec<String> = Vec::new();
    if git::ref_exists(&repo, "refs/heads/forge-verify").await {
        overlay.push("forge-verify".into());
    }
    let cfg_base = config::load_at(&repo, &dir, &base_sha).await?;
    // The most restricted egress among the tasks merged: a `model` level's
    // branch never gets a registry because it is verified alongside others.
    let trust = tasks
        .iter()
        .map(|t| t.trust)
        .find(|l| f.trust_policy(*l).egress == config::TrustEgress::Model)
        .unwrap_or_default();
    f.allow_egress(&dir, &cfg_base, trust);
    let mut steps = Vec::new();
    let mut outcome = IntegrateOutcome::Ready;
    for (completed, t) in tasks.iter().enumerate() {
        // Where the branch lives: the repository, else the remote, else the worktree.
        let src = if git::ref_exists(&repo, &format!("refs/heads/{}", t.branch)).await {
            repo.display().to_string()
        } else if let Some(url) = &remote_url
            && git::remote_branch_exists(url, &t.branch).await
        {
            url.clone()
        } else if Path::new(&t.worktree).join(".git").exists() {
            t.worktree.clone()
        } else {
            bail!(
                "task {}'s branch {} is nowhere: not in the repository, the remote, or a worktree",
                t.id,
                t.branch
            );
        };
        git::fetch_ref(&dir, &src, &t.branch)
            .await
            .with_context(|| format!("fetching {} from {src}", t.branch))?;
        let message = format!("Integrate task {}: {}", t.id, t.branch);
        let merged = match git::merge(&dir, "FETCH_HEAD", &message).await? {
            git::Merge::UpToDate => None,
            git::Merge::Merged(sha) => Some(sha),
            git::Merge::Conflict(files) => {
                outcome = IntegrateOutcome::Conflict {
                    task_id: t.id,
                    files,
                    completed,
                };
                break;
            }
        };
        steps.push(match &merged {
            Some(sha) => IntegrateStep::Merged {
                task_id: t.id,
                branch: t.branch.clone(),
                sha: sha.clone(),
            },
            None => IntegrateStep::AlreadyContained { task_id: t.id },
        });
        let own = format!("verify/{}", t.id);
        if git::ref_exists(&repo, &format!("refs/heads/{own}")).await {
            overlay.push(own);
        }
        let verdict = verify_merged_tree(
            f,
            MergeVerifyArgs {
                task_id: t.id,
                repo: &repo,
                worktree: &dir,
                base_sha: &base_sha,
                branch: &branch,
                cfg: &cfg_base,
                task_checks: &[],
                allow_protected: true,
                overlay_refs: &overlay,
            },
        )
        .await
        .map_err(|e| match e {
            Fault::Task(e) | Fault::Env(e) => e,
        })?;
        if verdict.state != AttemptState::Succeeded {
            outcome = IntegrateOutcome::VerifyFailed {
                task_id: t.id,
                reason: verdict.reason,
                completed: completed + 1,
            };
            break;
        }
        steps.push(IntegrateStep::Verified { task_id: t.id });
    }
    if matches!(outcome, IntegrateOutcome::Ready) {
        git::push_to_repo(&f.paths.home, &repo, &dir, &branch).await?;
        let _ = std::fs::remove_dir_all(&dir);
        crate::sandbox::discard_provider_state(&dir);
    }
    Ok(IntegrateReport {
        base_branch: cfg.base_branch,
        base_sha,
        repo,
        branch,
        dir,
        steps,
        outcome,
    })
}

/// Whether a blocked task's last attempt, though it did not settle,
/// still leaves a branch worth landing: a review demotion the operator
/// or the supervisor set aside, naming no defect the task requires
/// fixing, or a question whose L1 checks already ran on a clean,
/// committed tree and all passed. The question or the demotion stands
/// either way; neither says the commit itself is bad.
pub(crate) fn landable_needs_input(f: &Forge, t: &Task) -> Result<bool> {
    if t.state != TaskState::Blocked {
        return Ok(false);
    }
    Ok(f.store
        .attempts(t.id)?
        .iter()
        .rev()
        .find(|a| a.is_agent())
        .is_some_and(|a| {
            a.state == crate::store::AttemptState::NeedsInput
                && (a.reason.starts_with("review demoted")
                    || crate::verify::l1_all_passed(
                        &serde_json::from_str::<Vec<crate::checks::CheckResult>>(&a.verdict_json)
                            .unwrap_or_default(),
                    ))
        }))
}

/// Land a task's verified branch on the base: a verified task, or one
/// blocked on a review demotion or a question that a human or the
/// supervisor set aside (see `landable_needs_input`). `by_hand` is true
/// only for the operator's own `forge land`, never for the supervisor's
/// automated accept-and-land (see `Task::hand_landed`, one of the
/// human-attention signals). Returns the line to print.
pub(crate) async fn land_task(f: &Forge, id: i64, by_hand: bool) -> Result<String> {
    let Some(mut t) = f.store.task(id)? else {
        bail!("no task {id}");
    };
    let demoted = landable_needs_input(f, &t)?;
    if t.state != TaskState::Succeeded && t.state != TaskState::Unverified && !demoted {
        bail!(
            "task {id} is {}; only a verified task lands",
            t.state.as_str()
        );
    }
    if !t.landed_sha.is_empty() {
        bail!("task {id} already landed: {}", t.reason);
    }
    if !by_hand && !f.trust_policy(t.trust).auto_land {
        bail!(
            "task {id} is at trust {}, which does not land itself; land it with forge land {id}",
            t.trust.as_str()
        );
    }
    let repo = PathBuf::from(&t.repo);
    if !Path::new(&t.worktree).join(".git").exists() {
        bail!(
            "task {id}'s worktree is gone ({}); retry the task instead",
            t.worktree
        );
    }
    let cfg = config::load_working(&repo).await?;
    let Some(remote) = cfg.push_remote.clone() else {
        bail!("{} has no push remote; nothing to land on", repo.display());
    };
    let Some(url) = git::remote_url(&repo, &remote).await else {
        bail!("remote {remote} has no URL in {}", repo.display());
    };
    let mut seq = f.store.ops(id)?.len() as i64;
    let mut attempt_no = f.store.attempts(id)?.len() as i64;
    match crate::landing::integrate(f, &mut t, &url, &remote, &mut seq, &mut attempt_no)
        .await
        .map_err(|e| match e {
            crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
        })? {
        crate::landing::Integrate::Landed(sha) => {
            t.reason = format!("landed {} @ {}", t.base_branch, &sha[..sha.len().min(8)]);
            t.landed_sha = sha.clone();
            t.landed_at = Some(crate::unix_now());
            t.hand_landed = by_hand;
            t.pushed = true;
            if demoted {
                t.state = TaskState::Succeeded;
                t.finished_at = Some(crate::unix_now());
            }
            f.store.update_task(&t)?;
            crate::queue::settle_superseded(f, id)?;
            if let Some(iid) = t.initiative {
                crate::view::maybe_settle_initiative(f, id, iid)?;
            }
            f.report.emit(
                id,
                crate::report::Event::TaskDone {
                    state: t.state.as_str(),
                    attempts: f.store.attempts(id)?.len(),
                    cost: f.store.task_cost(id)?,
                    reason: &t.reason,
                    branch: &t.branch,
                    pushed: true,
                    compare: None,
                    remove_cmd: "",
                },
            );
            Ok(format!(
                "landed task {id} on {} @ {}",
                t.base_branch,
                &sha[..8]
            ))
        }
        crate::landing::Integrate::Rewind { first, .. } => {
            // No need to store base_sha or reload cfg here: this command
            // only reports the conflict and exits without touching `t` or
            // `cfg` again. `forge retry` enqueues a brand-new task rather
            // than resuming this one, so the stale base_sha left on this
            // task is never read.
            bail!(
                "task {id} needs the coder again: {first}\n  forge retry {id} runs it through the integrator with the conflict as feedback"
            )
        }
        crate::landing::Integrate::Failed(reason) => bail!("task {id} could not land: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_shows_each_step_and_the_ff_only_instructions() {
        let report = IntegrateReport {
            base_branch: "main".into(),
            base_sha: "abc123456789".into(),
            repo: PathBuf::from("/repo"),
            branch: "forge/integration-1".into(),
            dir: PathBuf::from("/scratch"),
            steps: vec![
                IntegrateStep::Merged {
                    task_id: 1,
                    branch: "forge/task-1".into(),
                    sha: "deadbeef00".into(),
                },
                IntegrateStep::Verified { task_id: 1 },
                IntegrateStep::AlreadyContained { task_id: 2 },
                IntegrateStep::Verified { task_id: 2 },
            ],
            outcome: IntegrateOutcome::Ready,
        };
        assert_eq!(
            report.render(),
            "base     main @ abc12345\n\
             task 1    merged forge/task-1 as deadbeef\n\
             task 1    verified with everything before it\n\
             task 2    already contained\n\
             task 2    verified with everything before it\n\
             integrated 2 task(s) as forge/integration-1 in /repo\n  \
             git -C /repo merge --ff-only forge/integration-1"
        );
    }

    #[test]
    fn render_stops_at_a_conflict_and_names_the_scratch_clone() {
        let report = IntegrateReport {
            base_branch: "main".into(),
            base_sha: "abc123456789".into(),
            repo: PathBuf::from("/repo"),
            branch: "forge/integration-1".into(),
            dir: PathBuf::from("/scratch"),
            steps: vec![
                IntegrateStep::Merged {
                    task_id: 1,
                    branch: "forge/task-1".into(),
                    sha: "deadbeef00".into(),
                },
                IntegrateStep::Verified { task_id: 1 },
            ],
            outcome: IntegrateOutcome::Conflict {
                task_id: 3,
                files: vec!["answer.txt".into()],
                completed: 1,
            },
        };
        assert_eq!(
            report.render(),
            "base     main @ abc12345\n\
             task 1    merged forge/task-1 as deadbeef\n\
             task 1    verified with everything before it\n\
             task 3    CONFLICT in answer.txt\n\
             stopped after 1 task(s); the scratch clone is at /scratch"
        );
    }

    #[test]
    fn render_stops_when_the_merged_tree_fails_verification() {
        let report = IntegrateReport {
            base_branch: "main".into(),
            base_sha: "abc123456789".into(),
            repo: PathBuf::from("/repo"),
            branch: "forge/integration-1".into(),
            dir: PathBuf::from("/scratch"),
            steps: vec![IntegrateStep::Merged {
                task_id: 2,
                branch: "forge/task-2".into(),
                sha: "cafef00d00".into(),
            }],
            outcome: IntegrateOutcome::VerifyFailed {
                task_id: 2,
                reason: "shell check failed".into(),
                completed: 1,
            },
        };
        assert_eq!(
            report.render(),
            "base     main @ abc12345\n\
             task 2    merged forge/task-2 as cafef00d\n\
             task 2    checks FAIL after merging: shell check failed\n\
             stopped after 1 task(s); the scratch clone is at /scratch"
        );
    }
}
