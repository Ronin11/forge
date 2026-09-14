//! Landing: the integrator. After a task's last directive verifies, the
//! base is fetched, merged in if it moved, the merged tree verified with
//! the standing hidden suite, and the branch fast-forwarded onto the base
//! under a per-repository lock; a conflict or a failing check goes back to
//! the coder as a rewind. `forge land` runs the same function by hand.

use crate::ctx::Forge;
use crate::engine::{Classify, Fault, op};
use crate::report::Event;
use crate::store::{AttemptState, Task};
use crate::verify::{self, Subject};
use crate::{checks, config, git, unix_now};
use std::path::Path;
use std::time::Instant;

pub enum Integrate {
    /// On the base branch; its new tip.
    Landed(String),
    /// The coder has to act: a conflict with the moved base, or checks that
    /// fail with the base merged in. The feedback and its first line.
    Rewind { feedback: String, first: String },
    /// Nothing the coder can do about it.
    Failed(String),
}

/// One landing at a time per repository, across every worker process:
/// an advisory lock on a file under FORGE2_HOME, held until dropped.
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

/// Land the verified branch on the base branch: bring the base in, verify
/// everything with every hidden suite overlaid, push the branch, fast-forward
/// the base, and fold the task's hidden tests into `forge-verify`. Three
/// rows in the trace: `integrate`, `push`, `land`.
pub async fn integrate(
    f: &Forge,
    t: &mut Task,
    url: &str,
    remote: &str,
    seq: &mut i64,
) -> Result<Integrate, Fault> {
    let repo = Path::new(&t.repo);
    let wt = Path::new(&t.worktree);
    let _lock = repo_lock(f, repo).await?;
    let placed = format!("forge/{}", t.base_branch);
    for round in 0..3 {
        *seq += 1;
        let started = unix_now();
        let start = Instant::now();
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
                        *seq,
                        "integrate",
                        true,
                        started,
                        start,
                        false,
                        None,
                        &d,
                        None,
                        "",
                    )?;
                    return Ok(Integrate::Failed(d));
                }
            }
        } else {
            git::rev_parse(repo, &format!("refs/heads/{}", t.base_branch))
                .await
                .task()?
        };
        let mut detail = String::new();
        if main_sha != t.base_sha && !git::is_ancestor(wt, &main_sha, "HEAD").await {
            git::place_branch(repo, wt, &main_sha, &placed)
                .await
                .task()?;
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
                    let d = format!(
                        "{} moved to {}; conflicts in {}",
                        t.base_branch,
                        &main_sha[..8],
                        files.join(", ")
                    );
                    op(
                        f,
                        t.id,
                        *seq,
                        "integrate",
                        true,
                        started,
                        start,
                        false,
                        None,
                        &d,
                        None,
                        "",
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
                    return Ok(Integrate::Rewind { feedback, first: d });
                }
            }
        }
        // The branch contains the base as it is now: measure from there.
        if t.base_sha != main_sha && git::is_ancestor(wt, &main_sha, "HEAD").await {
            t.base_sha = main_sha.clone();
            f.store.update_task(t).env()?;
        }
        let cfg_now = config::load_at(repo, wt, &t.base_sha).await.task()?;
        let overlay = overlay_refs(repo, t.id, None).await;
        let v = verify::verify_integration(&Subject {
            task_id: t.id,
            repo,
            worktree: wt,
            base_sha: &t.base_sha,
            start_sha: &t.base_sha,
            cfg: &cfg_now,
            task_checks: &t.checks,
            paths: &[],
            allow_protected: t.allow_protected,
            overlay_refs: &overlay,
            pending_main: None,
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
        })
        .await
        .task()?;
        if v.state != AttemptState::Succeeded {
            let d = format!("{detail}{}", v.reason);
            op(
                f,
                t.id,
                *seq,
                "integrate",
                true,
                started,
                start,
                false,
                None,
                &d,
                None,
                "",
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
            });
        }
        op(
            f,
            t.id,
            *seq,
            "integrate",
            true,
            started,
            start,
            true,
            None,
            &format!(
                "{detail}verified against {} @ {}",
                t.base_branch,
                &t.base_sha[..8]
            ),
            None,
            "",
        )?;

        *seq += 1;
        let started = unix_now();
        let start = Instant::now();
        if let Err(e) = git::push(wt, url, &t.branch).await {
            let d = format!("push of {} failed: {e:#}", t.branch);
            op(
                f, t.id, *seq, "push", true, started, start, false, None, &d, None, "",
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
            f, t.id, *seq, "push", true, started, start, true, None, &t.branch, None, "",
        )?;

        *seq += 1;
        let started = unix_now();
        let start = Instant::now();
        if let Err(e) = git::push_head_to(wt, url, &t.base_branch).await {
            let d = format!("fast-forward of {} rejected: {e:#}", t.base_branch);
            op(
                f, t.id, *seq, "land", true, started, start, false, None, &d, None, "",
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
        let sha = git::head(wt).await.task()?;
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
                folded = match git::push(repo, url, "forge-verify").await {
                    Ok(()) => format!(
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
            *seq,
            "land",
            true,
            started,
            start,
            true,
            None,
            &format!("{} @ {}{folded}", t.base_branch, &sha[..8]),
            None,
            "",
        )?;
        f.report.emit(
            t.id,
            Event::Note {
                text: &format!("landed   {} @ {}{folded}", t.base_branch, &sha[..8]),
            },
        );
        return Ok(Integrate::Landed(sha));
    }
    unreachable!("the landing loop returns")
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
