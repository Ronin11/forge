//! Miss review: a class on every failure the mechanic hands to the
//! operator, and the twice rule (docs/WORKFLOWS.md, "Miss review";
//! docs/research/06-one-person-factories.md, 2.8). When the mechanic has
//! no follow-up for a failed task, Jev is asked which of four classes
//! the miss was: `packet-unclear` (the task text), `rule-gap` (a prompt,
//! rule, check or config that will fail again on tasks like it), `infra`
//! (the host or provider), or `one-off`. The answer is a `miss-review`
//! decision row citing the failure's cause key, a short deterministic
//! name for what failed (`check test`, `rule clean-tree`, `review`).
//!
//! The rule that makes it a loop: when two misses in the same repository
//! within seven days are rule gaps with the same cause key, the kernel
//! files one task to change one prompt line, rule or config value so it
//! does not recur, once per key per window (`miss-review-fix`, linked to
//! the task it filed). A classification is never a retry and never
//! reopens the failed task; with no `jev` provider configured nothing is
//! asked and nothing is recorded.

use crate::{
    agent::{self, Provider, Runner},
    checks::CheckResult,
    ctx::Forge,
    queue::{self, TaskRequest},
    report::Event,
    store::{InsertDecisionBy, Task},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

pub const KIND: &str = "miss-review";
pub const FIX_KIND: &str = "miss-review-fix";
pub const RULE_GAP: &str = "rule-gap";
/// How far back two rule gaps count as the same recurring one.
pub const WINDOW_SECS: i64 = 7 * 86_400;
/// How many rule gaps with one cause key file the fix task.
pub const TWICE: usize = 2;

const QUESTION: &str = "Why did this task fail, in one class? packet-unclear: the task text was vague, underspecified or contradictory, so right work could not be told from a wrong guess. rule-gap: a prompt, a rule, a check or a configuration value is missing or wrong, so the same failure will recur on other tasks like this one. infra: the host, the provider or the sandbox failed, not the work. one-off: a slip particular to this attempt that a plain retry would not repeat.";
const TIMEOUT: Duration = Duration::from_secs(30);
const TASK_CHARS: usize = 2000;
const TAIL_CHARS: usize = 400;
const MAX_ROWS: usize = 5;

/// A short, deterministic name for what failed, from the task's reason:
/// the first failing check or rule, `review`, `landing <...>`, else the
/// reason's first sixty characters. Two tasks failing the same way get
/// the same key, which is what the twice rule counts.
pub fn cause_key(reason: &str) -> String {
    let head = reason.trim();
    let head = head.split(" (after ").next().unwrap_or(head).trim();
    let first = |s: &str| s.split([',', ';']).next().unwrap_or(s).trim().to_string();
    if let Some(rest) = head
        .strip_prefix("L1 failed: ")
        .or_else(|| head.strip_prefix("L2 failed: "))
    {
        return format!("check {}", first(rest));
    }
    if let Some(rest) = head.strip_prefix("L0 failed: ") {
        return format!("rule {}", first(rest));
    }
    if let Some(rest) = head.strip_prefix("landing failed: ") {
        return format!("landing {}", cause_key(rest));
    }
    if head.starts_with("review demoted") {
        return "review".into();
    }
    head.chars().take(60).collect::<String>().to_lowercase()
}

fn provider(f: &Forge) -> Option<&Provider> {
    f.providers.values().find(|p| p.runner == Runner::Jev)
}

fn request(t: &Task, verdict: &[CheckResult], key: &str) -> Value {
    let task: String = t.task.chars().take(TASK_CHARS).collect();
    let rows: Vec<String> = verdict
        .iter()
        .filter(|c| !c.ok)
        .take(MAX_ROWS)
        .map(|c| {
            let tail: String = c.tail.chars().take(TAIL_CHARS).collect();
            format!("{} {}: {}", c.level, c.name, tail.trim())
        })
        .collect();
    let state = format!(
        "The task:\n{task}\n\nHow it failed: {}\nCause key: {key}\n\nWhat failed:\n{}",
        t.reason,
        if rows.is_empty() {
            "(no check rows; the agent or the kernel ended it)".to_string()
        } else {
            rows.join("\n")
        }
    );
    json!({"model": "", "state": state, "questions": {
        "class": {"type": "choice", "instructions": QUESTION, "criteria": {
            "packet-unclear": "the task text was vague, underspecified or contradictory",
            "rule-gap": "a prompt, rule, check or config value is missing or wrong and the failure will recur on tasks like this one",
            "infra": "the host, the provider or the sandbox failed, not the work",
            "one-off": "a slip particular to this attempt",
        }}
    }})
}

/// Classify the miss on `t`, a task the mechanic just handed to the
/// operator, and apply the twice rule. `Some(id)` is the fix task the
/// rule filed; `None` is every other outcome, including no provider.
pub async fn review(f: &Forge, t: &Task, verdict: &[CheckResult]) -> Result<Option<i64>> {
    let Some(p) = provider(f) else {
        return Ok(None);
    };
    let key = cause_key(&t.reason);
    let answers = match agent::ask(p, &request(t, verdict, &key), TIMEOUT).await {
        Ok((a, _)) => a,
        Err(e) => {
            f.report.emit(
                t.id,
                Event::Note {
                    text: &format!("miss review: Jev could not be asked: {e:#}"),
                },
            );
            return Ok(None);
        }
    };
    let Some(a) = agent::read_answer(&answers["class"]) else {
        return Ok(None);
    };
    let p_class = a.probabilities[&a.label].as_f64().unwrap_or(0.0);
    let id = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: QUESTION,
        answer: &format!("{} (p {p_class:.2})", a.label),
        answered_by: "jev",
        citations: &format!("cause {key}; {}", a.probabilities),
        answered_for: None,
    })?;
    f.store.set_decision_kind(id, KIND)?;
    f.report.emit(
        t.id,
        Event::Note {
            text: &format!("miss review: {} (p {p_class:.2}), cause {key}", a.label),
        },
    );
    if a.label != RULE_GAP {
        return Ok(None);
    }
    twice(f, t, &key).await
}

/// The twice rule on `key` in `t`'s repository: file the one fix task
/// when `TWICE` rule gaps share it within the window and none was filed
/// for it yet.
async fn twice(f: &Forge, t: &Task, key: &str) -> Result<Option<i64>> {
    let since = crate::unix_now() - WINDOW_SECS;
    let prefix = format!("cause {key};");
    let mut tasks: Vec<i64> = f
        .store
        .decisions_of_kind_since(KIND, since)?
        .into_iter()
        .filter(|d| {
            d.repo == t.repo && d.answer.starts_with(RULE_GAP) && d.citations.starts_with(&prefix)
        })
        .filter_map(|d| d.task_id)
        .collect();
    tasks.sort_unstable();
    tasks.dedup();
    if tasks.len() < TWICE {
        return Ok(None);
    }
    let filed = f
        .store
        .decisions_of_kind_since(FIX_KIND, since)?
        .iter()
        .any(|d| d.repo == t.repo && d.citations.starts_with(&prefix));
    if filed {
        return Ok(None);
    }
    let list = tasks
        .iter()
        .map(|id| format!("task {id}"))
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!(
        "Rule gap: {key} failed {} tasks in the last 7 days ({list}) and the miss review called each a rule gap, not the task's own slip. Change one prompt line, one rule or one config value so it does not recur; do not fix those tasks. Read what each failed on (forge show <id>, forge journal <id>), find the one thing that would have made both pass, or fail earlier with a clearer reason, and change only that. Say in the result which line changed and why it covers both.",
        tasks.len()
    );
    let req = TaskRequest {
        repo: PathBuf::from(&t.repo),
        task: text,
        project: t.project.clone(),
        max_turns: 100,
        retries: 1,
        timeout_secs: 1800,
        ..Default::default()
    };
    let n = queue::enqueue(f, &req, None).await?;
    let id = f.store.insert_decision_by(InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("rule gap {key} recurred: {list}"),
        answer: &format!("filed task {}", n.id),
        answered_by: "mechanic",
        citations: &format!("{prefix} {list}"),
        answered_for: None,
    })?;
    f.store.set_decision_kind(id, FIX_KIND)?;
    f.store.set_decision_retry(id, n.id)?;
    f.report.emit(
        t.id,
        Event::Note {
            text: &format!(
                "miss review: rule gap {key} twice ({list}); filed task {} to close it",
                n.id
            ),
        },
    );
    Ok(Some(n.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::tests::{Reply, choice, fake, forge};
    use crate::store::TaskState;
    use std::sync::Arc;

    #[test]
    fn the_cause_key_names_the_first_failing_check_or_rule() {
        assert_eq!(
            cause_key("L1 failed: test (after 2 attempt(s))"),
            "check test"
        );
        assert_eq!(
            cause_key("L2 failed: task-check-1, task-check-2"),
            "check task-check-1"
        );
        assert_eq!(
            cause_key("L0 failed: clean-tree, has-commits"),
            "rule clean-tree"
        );
        assert_eq!(
            cause_key("landing failed: L1 failed: test"),
            "landing check test"
        );
        assert_eq!(cause_key("review demoted: answer.txt is 42"), "review");
        assert_eq!(
            cause_key("Agent Exit 1: Something Odd"),
            "agent exit 1: something odd"
        );
    }

    fn class(label: &'static str, p: f64) -> Reply {
        Arc::new(move |_| (200, json!({"class": choice(label, p)})))
    }

    /// A git repository with a forge.toml, which `queue::enqueue` needs.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.email", "a@a.com"]);
        git(&["config", "user.name", "a"]);
        std::fs::write(
            dir.path().join("forge.toml"),
            "[checks]\ntest = [\"true\"]\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "base"]);
        dir
    }

    fn failed(f: &Forge, repo: &std::path::Path, reason: &str) -> Task {
        let mut t = Task {
            repo: repo.display().to_string(),
            task: "do the thing".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 10,
            max_attempts: 1,
            timeout_secs: 60,
            state: TaskState::Failed,
            reason: reason.into(),
            created_at: crate::unix_now(),
            workflow: "direct".into(),
            ..Default::default()
        };
        t.id = f.store.insert_task(&t).unwrap();
        t
    }

    #[tokio::test]
    async fn a_rule_gap_twice_on_one_cause_files_one_fix_task_in_the_window() {
        let (port, seen) = fake(class("rule-gap", 0.88)).await;
        let (_d, f) = forge(Some(port), "");
        let r = repo();
        let a = failed(&f, r.path(), "L1 failed: test (after 2 attempt(s))");
        assert_eq!(review(&f, &a, &[]).await.unwrap(), None);
        let d = &f.store.decisions_of_kind_since(KIND, 0).unwrap()[0];
        assert_eq!(d.answer, "rule-gap (p 0.88)");
        assert!(
            d.citations.starts_with("cause check test;"),
            "{}",
            d.citations
        );
        let state = seen.lock().unwrap()[0]["state"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(state.contains("Cause key: check test"), "{state}");
        // The second rule gap with the same key files the fix, once.
        let b = failed(&f, r.path(), "L1 failed: test, clippy (after 1 attempt(s))");
        let fix = review(&f, &b, &[])
            .await
            .unwrap()
            .expect("the twice rule files");
        let filed = f.store.task(fix).unwrap().unwrap();
        assert_eq!(filed.state, TaskState::Queued);
        assert!(
            filed
                .task
                .starts_with("Rule gap: check test failed 2 tasks"),
            "{}",
            filed.task
        );
        assert!(
            filed
                .task
                .contains(&format!("task {}, task {}", a.id, b.id))
        );
        let fixes = f.store.decisions_of_kind_since(FIX_KIND, 0).unwrap();
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].retry_id, Some(fix));
        let c = failed(&f, r.path(), "L1 failed: test (after 1 attempt(s))");
        assert_eq!(
            review(&f, &c, &[]).await.unwrap(),
            None,
            "one fix per key per window"
        );
        // A different key starts its own count.
        let d = failed(&f, r.path(), "L0 failed: clean-tree");
        assert_eq!(review(&f, &d, &[]).await.unwrap(), None);
        assert_eq!(f.store.decisions_of_kind_since(KIND, 0).unwrap().len(), 4);
    }

    #[tokio::test]
    async fn a_one_off_is_recorded_and_files_nothing_and_no_provider_asks_nothing() {
        let (port, _) = fake(class("one-off", 0.7)).await;
        let (_d, f) = forge(Some(port), "");
        let r = repo();
        for _ in 0..3 {
            let t = failed(&f, r.path(), "L1 failed: test");
            assert_eq!(review(&f, &t, &[]).await.unwrap(), None);
        }
        assert_eq!(f.store.decisions_of_kind_since(KIND, 0).unwrap().len(), 3);
        assert!(
            f.store
                .decisions_of_kind_since(FIX_KIND, 0)
                .unwrap()
                .is_empty()
        );
        let (_d2, f2) = forge(None, "");
        let t = failed(&f2, r.path(), "L1 failed: test");
        assert_eq!(review(&f2, &t, &[]).await.unwrap(), None);
        assert!(
            f2.store
                .decisions_of_kind_since(KIND, 0)
                .unwrap()
                .is_empty()
        );
    }
}
