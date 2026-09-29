//! The three write verbs and the gate in front of them. A write tool never
//! acts when the model calls it: `propose` checks the arguments against
//! the record and returns the sentence the operator will read. Only
//! `confirm`, called by the operator through `forge chat confirm` or the
//! web client's button, runs the action — once, through the same queue
//! functions the CLI's own `add`, `answer` and `retry` use — and records
//! it as a decision.

use super::reads::{cut, int, opt_int, opt_str};
use super::record::{self, Status, ToolCall};
use crate::ctx::Forge;
use crate::queue::{self, RetryOverrides, TaskRequest};
use crate::store::{InsertDecisionBy, NewChatTurn, TaskState};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The kind a decision recorded from a confirmed action carries.
pub const DECISION_KIND: &str = "chat-action";

const MAX_TASK_TEXT: usize = 8000;

/// Check a write tool's arguments and describe what confirming would do:
/// the arguments in the normal form `execute` reads, and the summary.
pub fn propose(f: &Forge, tool: &str, args: &Value) -> Result<(Value, String)> {
    match tool {
        "add_task" => propose_add(f, args),
        "answer_question" => propose_answer(f, args),
        "retry_task" => propose_retry(f, args),
        other => bail!("{other} is not a write tool"),
    }
}

fn known_workflow(f: &Forge, name: &str) -> Result<()> {
    match crate::workflows::get(&f.paths.home, name)? {
        Some(_) => Ok(()),
        None => bail!("no workflow {name:?}; `forge workflows list` names them"),
    }
}

fn propose_add(f: &Forge, args: &Value) -> Result<(Value, String)> {
    let text = opt_str(args, "task").context("`task` is the text of the task and is required")?;
    if text.chars().count() > MAX_TASK_TEXT {
        bail!("`task` is over {MAX_TASK_TEXT} characters; file it shorter");
    }
    let initiative = opt_int(args, "initiative")?;
    let mut project = opt_str(args, "project").map(str::to_string);
    if let Some(id) = initiative {
        let ini = f
            .store
            .initiative(id)?
            .ok_or_else(|| anyhow!("no initiative {id}"))?;
        if project.as_deref().is_some_and(|p| p != ini.project) {
            bail!("initiative {id} belongs to project {}", ini.project);
        }
        project = Some(ini.project);
    }
    let repo = match (opt_str(args, "repo"), &project) {
        // Only a repository a project lists: the model does not choose paths.
        (Some(repo), _) => {
            let listed = f.store.projects_listing_repo(repo)?;
            if listed.is_empty() {
                bail!("{repo} is not a repository any project lists");
            }
            if project.is_none() {
                project = listed.first().cloned();
            }
            repo.to_string()
        }
        (None, Some(p)) => f
            .store
            .first_repo(p)?
            .with_context(|| format!("project {p} lists no repository"))?,
        (None, None) => bail!("give the `project` (or `initiative`) the task belongs to"),
    };
    let workflow = opt_str(args, "workflow");
    if let Some(w) = workflow {
        known_workflow(f, w)?;
    }
    let after: Vec<i64> = args["after"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect();
    for d in &after {
        if f.store.task(*d)?.is_none() {
            bail!("`after` names task {d}, which does not exist");
        }
    }
    let summary = format!(
        "File a task in project {} (workflow {}{}): {}",
        project.as_deref().unwrap_or("?"),
        workflow.unwrap_or("the project's default"),
        initiative.map_or(String::new(), |i| format!(", initiative {i}")),
        cut(text, 300)
    );
    let normal = json!({
        "task": text, "project": project, "repo": repo, "workflow": workflow,
        "initiative": initiative, "after": after,
    });
    Ok((normal, summary))
}

fn propose_answer(f: &Forge, args: &Value) -> Result<(Value, String)> {
    let id = int(args, "task")?;
    let answer = opt_str(args, "answer").context("`answer` is required")?;
    let t = f.store.task(id)?.ok_or_else(|| anyhow!("no task {id}"))?;
    if t.state != TaskState::Blocked {
        bail!(
            "task {id} is {}; only a blocked task has a question to answer",
            t.state.as_str()
        );
    }
    let summary = format!(
        "Answer task {id}'s question ({}) with: {}",
        cut(&t.reason, 200),
        cut(answer, 300)
    );
    Ok((json!({"task": id, "answer": answer}), summary))
}

fn propose_retry(f: &Forge, args: &Value) -> Result<(Value, String)> {
    let id = int(args, "task")?;
    let t = f.store.task(id)?.ok_or_else(|| anyhow!("no task {id}"))?;
    if matches!(t.state, TaskState::Queued | TaskState::Running) {
        bail!(
            "task {id} is {}; only a finished task is retried",
            t.state.as_str()
        );
    }
    let workflow = opt_str(args, "workflow");
    if let Some(w) = workflow {
        known_workflow(f, w)?;
    }
    let again = args["again"].as_bool().unwrap_or(false);
    let summary = format!(
        "Retry task {id} ({}: {}) as a new task{}",
        t.state.as_str(),
        cut(&t.reason, 150),
        workflow.map_or(String::new(), |w| format!(" on workflow {w}"))
    );
    Ok((
        json!({"task": id, "workflow": workflow, "again": again}),
        summary,
    ))
}

/// Run a confirmed action. What it returns is the outcome sentence.
async fn execute(f: &Forge, session: i64, summary: &str, call: &ToolCall) -> Result<String> {
    let a = &call.arguments;
    let cite = format!("chat session {session}");
    match call.tool.as_str() {
        "add_task" => {
            let req = TaskRequest {
                repo: PathBuf::from(opt_str(a, "repo").context("no repo")?),
                task: opt_str(a, "task").context("no task")?.to_string(),
                project: opt_str(a, "project").map(str::to_string),
                workflow: opt_str(a, "workflow").map(str::to_string),
                initiative: opt_int(a, "initiative")?,
                after: a["after"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_i64)
                    .collect(),
                max_turns: 100,
                retries: 1,
                timeout_secs: 1800,
                ..Default::default()
            };
            let t = queue::enqueue(f, &req, None).await?;
            decide(f, &t, summary, &cite, &format!("filed task {}", t.id))?;
            Ok(format!("filed task {} ({})", t.id, t.workflow))
        }
        "answer_question" => {
            let id = int(a, "task")?;
            let text = opt_str(a, "answer").context("no answer")?;
            let (_, n) = queue::answer(f, id, text, "operator", &cite, None).await?;
            Ok(if n.id == id {
                format!("answered task {id}: {}", n.reason)
            } else {
                format!("answered task {id} as task {}", n.id)
            })
        }
        "retry_task" => {
            let id = int(a, "task")?;
            let old = f.store.task(id)?.ok_or_else(|| anyhow!("no task {id}"))?;
            if matches!(old.state, TaskState::Queued | TaskState::Running) {
                bail!(
                    "task {id} is {}; only a finished task is retried",
                    old.state.as_str()
                );
            }
            queue::refuse_live_descendant(f, id, a["again"].as_bool().unwrap_or(false))?;
            let none = std::collections::HashMap::new();
            let after = old
                .after
                .iter()
                .map(|&d| queue::map_dep(f, d, &none))
                .collect::<Result<Vec<_>>>()?;
            let o = RetryOverrides {
                workflow: opt_str(a, "workflow").map(str::to_string),
                ..RetryOverrides::none()
            };
            let req = queue::retry_request(&old, &o, true, after, None);
            let n = queue::enqueue(f, &req, Some(id)).await?;
            decide(
                f,
                &n,
                summary,
                &cite,
                &format!("retried task {id} as {}", n.id),
            )?;
            Ok(format!("retried task {id} as {}", n.id))
        }
        other => bail!("{other} is not a write tool"),
    }
}

/// The decision row a confirmed add or retry leaves on the task it made.
fn decide(
    f: &Forge,
    t: &crate::store::Task,
    summary: &str,
    cite: &str,
    outcome: &str,
) -> Result<()> {
    let id = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("Ask Forge proposed: {summary}"),
        answer: &format!("confirmed by the operator: {outcome}"),
        answered_by: "operator",
        citations: cite,
        answered_for: None,
    })?;
    f.store.set_decision_kind(id, DECISION_KIND)?;
    Ok(())
}

/// How an action was decided.
#[derive(Debug)]
pub struct Decided {
    pub action: String,
    pub status: Status,
    /// The outcome sentence, or the failure.
    pub outcome: String,
    /// The `action` turn recording it in the session.
    pub turn: i64,
}

/// The turn, its calls, and the proposal at `action`'s position, if it is
/// still waiting for a decision.
fn pending(f: &Forge, action: &str) -> Result<(crate::store::ChatTurn, Vec<ToolCall>, usize)> {
    let (turn_id, index) = record::parse_action_id(action)
        .with_context(|| format!("{action:?} is not an action id (turn.position, like 12.0)"))?;
    let turn = f
        .store
        .chat_turn(turn_id)?
        .ok_or_else(|| anyhow!("no action {action}"))?;
    let calls = record::parse_calls(&turn.tool_calls);
    let status = calls
        .get(index)
        .and_then(|c| c.proposal.as_ref())
        .map(|p| p.status)
        .ok_or_else(|| anyhow!("no action {action}"))?;
    if status != Status::Proposed {
        bail!("action {action} is already {}", status.as_str());
    }
    Ok((turn, calls, index))
}

/// Set the proposal at `index` to `status`/`outcome` on the turn's stored
/// calls, only if the stored calls are still `before`. Whether it won.
fn settle(
    f: &Forge,
    turn: i64,
    before: &str,
    calls: &mut [ToolCall],
    index: usize,
    status: Status,
    outcome: &str,
) -> Result<bool> {
    if let Some(p) = calls[index].proposal.as_mut() {
        p.status = status;
        p.outcome = outcome.to_string();
        p.decided_at = Some(crate::unix_now());
    }
    f.store
        .swap_chat_tool_calls(turn, before, &record::calls_json(calls))
}

fn record_decision(f: &Forge, session: i64, text: &str) -> Result<i64> {
    f.store.insert_chat_turn(&NewChatTurn {
        session,
        role: "action",
        text,
        tool_calls: "[]",
        cost_usd: 0.0,
        provider: "",
        model: "",
        prompt_hash: "",
    })
}

/// The operator's yes. Claims the proposal first — two confirmations of
/// one action race on the stored calls and exactly one wins — then runs
/// it and records what happened. A failing action is recorded `failed`
/// and is not run again.
pub async fn confirm(f: &Forge, action: &str) -> Result<Decided> {
    let (turn, mut calls, index) = pending(f, action)?;
    let claimed = settle(
        f,
        turn.id,
        &turn.tool_calls,
        &mut calls,
        index,
        Status::Confirmed,
        "running",
    )?;
    if !claimed {
        bail!("action {action} was decided by someone else first");
    }
    let summary = calls[index]
        .proposal
        .as_ref()
        .map(|p| p.summary.clone())
        .unwrap_or_default();
    let (status, outcome) = match execute(f, turn.session, &summary, &calls[index]).await {
        Ok(o) => (Status::Confirmed, o),
        Err(e) => (Status::Failed, format!("{e:#}")),
    };
    // Nobody else may write these calls now: the claim above was ours.
    let current = f.store.chat_turn(turn.id)?.context("the turn is gone")?;
    if let Some(p) = calls[index].proposal.as_mut() {
        p.status = status;
        p.outcome = outcome.clone();
    }
    f.store
        .swap_chat_tool_calls(turn.id, &current.tool_calls, &record::calls_json(&calls))?;
    let text = match status {
        Status::Failed => format!("Confirmed, but it failed: {outcome}"),
        _ => format!("Confirmed: {outcome}"),
    };
    let recorded = record_decision(f, turn.session, &text)?;
    Ok(Decided {
        action: action.to_string(),
        status,
        outcome,
        turn: recorded,
    })
}

/// The operator's no: nothing runs.
pub fn reject(f: &Forge, action: &str) -> Result<Decided> {
    let (turn, mut calls, index) = pending(f, action)?;
    let summary = calls[index]
        .proposal
        .as_ref()
        .map(|p| p.summary.clone())
        .unwrap_or_default();
    if !settle(
        f,
        turn.id,
        &turn.tool_calls,
        &mut calls,
        index,
        Status::Rejected,
        "rejected by the operator",
    )? {
        bail!("action {action} was decided by someone else first");
    }
    let recorded = record_decision(f, turn.session, &format!("Rejected: {summary}"))?;
    Ok(Decided {
        action: action.to_string(),
        status: Status::Rejected,
        outcome: "rejected by the operator".into(),
        turn: recorded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::Paths;
    use crate::store::{Store, Task};

    fn fixture() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        (dir, Forge::open_with(paths, store).unwrap())
    }

    /// A real repository the queue accepts a task in.
    fn repo(dir: &std::path::Path) -> std::path::PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("forge.toml"), "[checks]\nok = [\"true\"]\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "-A"],
            &["commit", "-q", "-m", "init"],
        ] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
        repo
    }

    fn failed_task(f: &Forge, repo: &str) -> i64 {
        f.store
            .insert_task(&Task {
                repo: repo.into(),
                task: "make the thing".into(),
                base_branch: "main".into(),
                model: "m".into(),
                max_turns: 1,
                max_attempts: 1,
                timeout_secs: 60,
                state: TaskState::Failed,
                reason: "test failed".into(),
                workflow: "direct".into(),
                created_at: crate::unix_now(),
                ..Default::default()
            })
            .unwrap()
    }

    /// An assistant turn that proposed `retry_task` on `task`; its action id.
    fn proposed_retry(f: &Forge, task: i64) -> (i64, String) {
        let session = f.store.create_chat_session("s", "p").unwrap();
        let (args, summary) = propose(f, "retry_task", &json!({"task": task})).unwrap();
        let calls = vec![
            ToolCall::read("task", json!({"id": task}), "", json!({})),
            ToolCall::proposed("retry_task", args, "", summary),
        ];
        let turn = f
            .store
            .insert_chat_turn(&NewChatTurn {
                session,
                role: "assistant",
                text: "I propose a retry.",
                tool_calls: &record::calls_json(&calls),
                cost_usd: 0.0,
                provider: "p",
                model: "m",
                prompt_hash: "h",
            })
            .unwrap();
        (session, record::action_id(turn, 1))
    }

    fn status_of(f: &Forge, action: &str) -> (Status, String) {
        let (turn, index) = record::parse_action_id(action).unwrap();
        let t = f.store.chat_turn(turn).unwrap().unwrap();
        let p = record::parse_calls(&t.tool_calls)[index]
            .proposal
            .clone()
            .unwrap();
        (p.status, p.outcome)
    }

    #[tokio::test]
    async fn nothing_runs_until_the_operator_confirms() {
        let (dir, f) = fixture();
        let repo = repo(dir.path());
        let old = failed_task(&f, &repo.display().to_string());
        let (session, action) = proposed_retry(&f, old);
        assert_eq!(
            f.store.queued_count().unwrap(),
            0,
            "proposing queued nothing"
        );
        assert_eq!(status_of(&f, &action).0, Status::Proposed);

        let d = confirm(&f, &action).await.unwrap();
        assert_eq!(d.status, Status::Confirmed, "{}", d.outcome);
        assert_eq!(f.store.queued_count().unwrap(), 1);
        assert!(
            d.outcome.contains(&format!("retried task {old} as")),
            "{}",
            d.outcome
        );
        assert_eq!(status_of(&f, &action).0, Status::Confirmed);

        // The session records the decision, and so does the decisions table.
        let turns = f.store.chat_turns(session).unwrap();
        let last = turns.last().unwrap();
        assert_eq!((last.role.as_str(), last.id), ("action", d.turn));
        assert!(
            last.text.starts_with("Confirmed: retried task"),
            "{}",
            last.text
        );
        let decisions = f.store.decisions(&Default::default()).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].kind, DECISION_KIND);
        assert!(
            decisions[0]
                .citations
                .contains(&format!("chat session {session}"))
        );
    }

    #[tokio::test]
    async fn a_confirmed_action_cannot_be_confirmed_or_rejected_again() {
        let (dir, f) = fixture();
        let old = failed_task(&f, &repo(dir.path()).display().to_string());
        let (_, action) = proposed_retry(&f, old);
        confirm(&f, &action).await.unwrap();
        let e = confirm(&f, &action).await.unwrap_err().to_string();
        assert!(e.contains("already confirmed"), "{e}");
        let e = reject(&f, &action).unwrap_err().to_string();
        assert!(e.contains("already confirmed"), "{e}");
        assert_eq!(f.store.queued_count().unwrap(), 1, "it ran exactly once");
    }

    #[tokio::test]
    async fn a_rejected_action_never_runs() {
        let (dir, f) = fixture();
        let old = failed_task(&f, &repo(dir.path()).display().to_string());
        let (session, action) = proposed_retry(&f, old);
        let d = reject(&f, &action).unwrap();
        assert_eq!(d.status, Status::Rejected);
        assert_eq!(status_of(&f, &action).0, Status::Rejected);
        let e = confirm(&f, &action).await.unwrap_err().to_string();
        assert!(e.contains("already rejected"), "{e}");
        assert_eq!(f.store.queued_count().unwrap(), 0);
        assert!(f.store.decisions(&Default::default()).unwrap().is_empty());
        let last = f.store.chat_turns(session).unwrap().pop().unwrap();
        assert!(last.text.starts_with("Rejected:"), "{}", last.text);
    }

    #[tokio::test]
    async fn an_action_that_fails_is_recorded_failed_and_is_not_run_again() {
        let (_dir, f) = fixture();
        let old = failed_task(&f, "/nowhere/at/all");
        let (_, action) = proposed_retry(&f, old);
        let d = confirm(&f, &action).await.unwrap();
        assert_eq!(d.status, Status::Failed);
        assert_eq!(status_of(&f, &action).0, Status::Failed);
        let e = confirm(&f, &action).await.unwrap_err().to_string();
        assert!(e.contains("already failed"), "{e}");
        assert!(f.store.decisions(&Default::default()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_a_proposal_can_be_decided() {
        let (dir, f) = fixture();
        let old = failed_task(&f, &repo(dir.path()).display().to_string());
        let (_, action) = proposed_retry(&f, old);
        let (turn, _) = record::parse_action_id(&action).unwrap();
        // Position 0 is a read tool's call, not a proposal; 7 is past the end.
        for bad in [
            format!("{turn}.0"),
            format!("{turn}.7"),
            "9999.0".into(),
            "nope".into(),
        ] {
            assert!(confirm(&f, &bad).await.is_err(), "{bad}");
            assert!(reject(&f, &bad).is_err(), "{bad}");
        }
        assert_eq!(f.store.queued_count().unwrap(), 0);
        assert_eq!(status_of(&f, &action).0, Status::Proposed);
    }

    #[test]
    fn of_two_racing_decisions_only_the_first_to_swap_wins() {
        let (dir, f) = fixture();
        let old = failed_task(&f, &repo(dir.path()).display().to_string());
        let (_, action) = proposed_retry(&f, old);
        let (turn, index) = record::parse_action_id(&action).unwrap();
        let stale = f.store.chat_turn(turn).unwrap().unwrap();
        let mut a = record::parse_calls(&stale.tool_calls);
        let mut b = a.clone();
        assert!(
            settle(
                &f,
                turn,
                &stale.tool_calls,
                &mut a,
                index,
                Status::Rejected,
                "x"
            )
            .unwrap()
        );
        assert!(
            !settle(
                &f,
                turn,
                &stale.tool_calls,
                &mut b,
                index,
                Status::Confirmed,
                "y"
            )
            .unwrap()
        );
        assert_eq!(status_of(&f, &action).0, Status::Rejected);
    }

    #[test]
    fn a_proposal_for_a_missing_workflow_is_refused_when_it_is_made() {
        let (_dir, f) = fixture();
        let old = failed_task(&f, "/repo");
        let e = propose(
            &f,
            "retry_task",
            &json!({"task": old, "workflow": "no-such"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("no workflow"), "{e}");
    }
}
