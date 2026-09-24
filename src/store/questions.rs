use super::*;

/// How a question that blocked a task ended. `Open` is still waiting on a
/// person; the others carry the unix second it was settled at.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Open,
    Supervisor { at: i64 },
    Operator { at: i64 },
    Withdrawn { at: i64 },
}

/// The task an answer re-queued: what the classifier of "do it as stated"
/// reads to see whether the lineage's next task was the same ask plus the
/// answer, and whether it landed.
#[derive(Debug, Clone)]
pub struct RetryFacts {
    pub task: String,
    pub landed: bool,
}

/// One task that blocked with a question, and how (or whether) it was
/// settled: the raw material of `forge stats --questions`.
#[derive(Debug, Clone)]
pub struct QuestionRecord {
    /// `review`, `question`, `workflow`, `job` or `dependency`.
    pub kind: &'static str,
    pub blocked_at: i64,
    pub question: String,
    pub task: String,
    pub resolution: Resolution,
    /// The answer text, when a decision recorded one.
    pub answer: Option<String>,
    pub retry: Option<RetryFacts>,
}

impl Store {
    /// Every task that blocked with a question at or after `since` (unix
    /// seconds; `None` is all time), oldest first. A question is an agent
    /// attempt that ended `needs_input` (its reason label says whether it
    /// is a plain question, a workflow request or a review demotion), a
    /// job's blocked `job question` task, or a task blocked on a
    /// dependency. Found through attempts and decisions, not the task's
    /// current state, because answering can settle the task (an accepted
    /// demotion lands it) and withdrawing overwrites its reason.
    ///
    /// The time a question blocked is the blocking attempt's finish; a
    /// job question or dependency block has no attempt, so the task's
    /// `finished_at` (dependency) or `created_at` is used, and a
    /// withdrawn dependency block, whose `finished_at` the withdrawal
    /// overwrote, falls back to `created_at`.
    pub fn question_records(&self, since: Option<i64>) -> Result<Vec<QuestionRecord>> {
        let ids: Vec<i64> = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT t.id FROM tasks t
                 WHERE t.state IN ('blocked', 'withdrawn')
                    OR (t.task = 'job question' AND t.workflow = 'direct')
                    OR EXISTS (SELECT 1 FROM attempts a WHERE a.task_id = t.id AND a.state = 'needs_input')
                 ORDER BY t.id",
            )?;
            let rows = stmt.query_map([], |r| r.get("id"))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for id in ids {
            let Some(t) = self.task(id)? else { continue };
            let attempts = self.attempts(id)?;
            let asked = attempts
                .iter()
                .rev()
                .find(|a| a.is_agent() && a.state == AttemptState::NeedsInput);
            let withdraw_question: Option<String> = {
                let c = self.lock();
                c.query_row(
                    "SELECT question FROM decisions WHERE task_id = ?1 AND retry_id = ?1 ORDER BY id LIMIT 1",
                    params![id],
                    |r| r.get("question"),
                )
                .optional()?
            };
            let (kind, question, blocked_at) = if t.task == "job question" && t.workflow == "direct"
            {
                ("job", t.reason.clone(), t.created_at)
            } else if let Some(a) = asked {
                let (kind, text) = crate::view::request_kind(&a.reason);
                (kind, text, a.finished_at.unwrap_or(a.started_at))
            } else {
                let reason = match (&t.state, &withdraw_question) {
                    (TaskState::Withdrawn, Some(q)) => q.clone(),
                    (TaskState::Blocked, _) => t.reason.clone(),
                    _ => continue,
                };
                let at = if t.state == TaskState::Withdrawn {
                    t.created_at
                } else {
                    t.finished_at.unwrap_or(t.created_at)
                };
                if !reason.starts_with("waits on task") {
                    continue;
                }
                ("dependency", reason, at)
            };
            if !matches!(
                kind,
                "review" | "question" | "workflow" | "job" | "dependency"
            ) {
                continue;
            }
            if since.is_some_and(|s| blocked_at < s) {
                continue;
            }
            let decision: Option<(String, String, i64, Option<i64>)> = {
                let c = self.lock();
                c.query_row(
                    "SELECT answered_by, answer, created_at, retry_id FROM decisions
                     WHERE task_id = ?1 AND created_at >= ?2 AND question != ?3
                     ORDER BY id LIMIT 1",
                    params![id, blocked_at, format!("task {id}'s spec")],
                    |r| {
                        Ok((
                            r.get("answered_by")?,
                            r.get("answer")?,
                            r.get("created_at")?,
                            r.get("retry_id")?,
                        ))
                    },
                )
                .optional()?
            };
            let children: Vec<Task> = {
                let ids: Vec<i64> = {
                    let c = self.lock();
                    let mut stmt =
                        c.prepare("SELECT id FROM tasks WHERE retry_of = ?1 ORDER BY id")?;
                    let rows = stmt.query_map(params![id], |r| r.get("id"))?;
                    rows.collect::<rusqlite::Result<_>>()?
                };
                let mut v = Vec::new();
                for cid in ids {
                    v.extend(self.task(cid)?);
                }
                v
            };
            let child = match decision.as_ref().and_then(|d| d.3) {
                Some(rid) if rid != id => children.iter().find(|c| c.id == rid),
                _ => children.first(),
            };
            let resolution = match &decision {
                Some((_, _, at, Some(rid))) if *rid == id => Resolution::Withdrawn { at: *at },
                Some((by, _, at, _)) if by == "supervisor" => Resolution::Supervisor { at: *at },
                Some((_, _, at, _)) => Resolution::Operator { at: *at },
                None if t.state == TaskState::Withdrawn => Resolution::Withdrawn {
                    at: t.finished_at.unwrap_or(blocked_at),
                },
                None if t.hand_landed => Resolution::Operator {
                    at: t.landed_at.unwrap_or(blocked_at),
                },
                None => match children.first() {
                    Some(c) => Resolution::Operator { at: c.created_at },
                    None => Resolution::Open,
                },
            };
            if resolution == Resolution::Open && t.state != TaskState::Blocked {
                continue;
            }
            out.push(QuestionRecord {
                kind,
                blocked_at,
                question,
                task: t.task.clone(),
                resolution,
                answer: decision.map(|d| d.1),
                retry: child.map(|c| RetryFacts {
                    task: c.task.clone(),
                    landed: c.state == TaskState::Succeeded && !c.landed_sha.is_empty(),
                }),
            });
        }
        Ok(out)
    }
}
