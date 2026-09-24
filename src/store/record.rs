use super::*;

/// An operator's answer to a blocked task's question, or an operator-run
/// maintenance action not tied to any one task (`forge stats --reprice`,
/// see `Store::insert_reprice_decision`) — `task_id` is `None` for those.
pub struct Decision {
    pub id: i64,
    pub task_id: Option<i64>,
    pub repo: String,
    pub question: String,
    pub answer: String,
    pub created_at: i64,
    /// "operator", "supervisor", or a channel contact's name.
    pub answered_by: String,
    /// What the answer cited, comma-separated: paths, "task N", "decision N".
    pub citations: String,
    /// The task the answer re-queued, when known: its state is the
    /// answer's outcome.
    pub retry_id: Option<i64>,
    /// Who the question was addressed to, copied from the task's
    /// `question_to` at answer time; `None` means the operator.
    pub answered_for: Option<String>,
    /// `demotion-as-task` when the kernel's own rule made the ruling (see
    /// docs/WORKFLOWS.md); empty for every other decision.
    pub kind: String,
}

/// An external reference a plugin or the operator recorded on a task: the
/// pull request it landed as, the issue it came from.
pub struct TaskRef {
    pub id: i64,
    pub task_id: i64,
    pub kind: String,
    pub url: String,
    pub label: String,
    /// Who recorded it: "operator" by default, or a plugin's own name.
    pub by: String,
    pub created_at: i64,
}

pub(super) const DECISION_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "repo",
    "question",
    "answer",
    "created_at",
    "answered_by",
    "citations",
    "retry_id",
    "answered_for",
    "kind",
];

fn decision_from_row(r: &Row) -> rusqlite::Result<Decision> {
    Ok(Decision {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        repo: r.get("repo")?,
        question: r.get("question")?,
        answer: r.get("answer")?,
        created_at: r.get("created_at")?,
        answered_by: r.get("answered_by")?,
        citations: r.get("citations")?,
        retry_id: r.get("retry_id")?,
        answered_for: r.get("answered_for")?,
        kind: r.get("kind")?,
    })
}

pub(super) const TASK_REF_COLUMNS: &[&str] =
    &["id", "task_id", "kind", "url", "label", "by", "created_at"];

fn task_ref_from_row(r: &Row) -> rusqlite::Result<TaskRef> {
    Ok(TaskRef {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        kind: r.get("kind")?,
        url: r.get("url")?,
        label: r.get("label")?,
        by: r.get("by")?,
        created_at: r.get("created_at")?,
    })
}

impl Store {
    /// Record an answer to a blocked task's question. `answered_for` is
    /// who the question was addressed to (the task's `question_to` at
    /// answer time), copied here since the task it retries into carries
    /// no such field forward.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_decision_by(
        &self,
        task_id: i64,
        repo: &str,
        question: &str,
        answer: &str,
        answered_by: &str,
        citations: &str,
        answered_for: Option<&str>,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO decisions (task_id, repo, question, answer, created_at, answered_by, citations, answered_for) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![task_id, repo, question, answer, crate::unix_now(), answered_by, citations, answered_for],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn set_decision_retry(&self, decision_id: i64, retry_id: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE decisions SET retry_id=?2 WHERE id=?1",
            params![decision_id, retry_id],
        )?;
        Ok(())
    }

    /// Mark a decision as the kernel's own ruling of `kind`.
    pub fn set_decision_kind(&self, decision_id: i64, kind: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE decisions SET kind=?2 WHERE id=?1",
            params![decision_id, kind],
        )?;
        Ok(())
    }

    /// How many times the supervisor has answered within this piece of work.
    pub fn supervisor_answers_in_lineage(&self, task_id: i64) -> Result<u32> {
        // self.lineage() walks *down* the whole retry tree from the root, so
        // it counts every branch, not just task_id's own ancestor chain; a
        // task can share a retry_of with a sibling (see dependents_retries /
        // latest_retry_of), so this must not narrow to task_id's own chain.
        // Computed before locking below: lineage() takes the lock itself.
        let ids: Vec<i64> = self.lineage(task_id)?.iter().map(|l| l.id).collect();
        let c = self.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let n: i64 = c.query_row(
            &format!(
                "SELECT COUNT(*) FROM decisions WHERE task_id IN ({placeholders}) AND answered_by='supervisor'"
            ),
            rusqlite::params_from_iter(ids.iter()),
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Every decision recorded on `id` or any task it retries, oldest first.
    pub fn decisions_in_lineage(&self, id: i64) -> Result<Vec<Decision>> {
        let c = self.lock();
        let ids = lineage_ids(&c, id)?;
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM decisions d WHERE d.task_id IN ({placeholders})
             ORDER BY d.id",
            DECISION_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), decision_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every plugin recorded as enabled.
    pub fn enabled_plugins(&self) -> Result<std::collections::BTreeSet<String>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT name FROM plugins WHERE enabled = 1")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Set a plugin's enabled flag, recording when it was enabled (`None`
    /// when disabling).
    pub fn set_plugin_enabled(&self, name: &str, enabled: bool, at: i64) -> Result<()> {
        let c = self.lock();
        c.execute(
            "INSERT INTO plugins (name, enabled, enabled_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET enabled = excluded.enabled, enabled_at = excluded.enabled_at",
            params![name, enabled as i64, enabled.then_some(at)],
        )?;
        Ok(())
    }

    /// Recorded answers, newest first; narrowed by repository, project,
    /// and/or initiative when given. Project and initiative narrow
    /// through the task the decision was recorded on, since a decision
    /// carries no such column of its own.
    pub fn decisions(&self, q: &DecisionFilter) -> Result<Vec<Decision>> {
        let c = self.lock();
        let cols = DECISION_COLUMNS
            .iter()
            .map(|c| format!("d.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = c.prepare(&format!(
            "SELECT {cols}
             FROM decisions d LEFT JOIN tasks t ON t.id = d.task_id
             WHERE (?1 IS NULL OR d.repo = ?1)
               AND (?2 IS NULL OR t.project = ?2)
               AND (?3 IS NULL OR t.initiative = ?3)
               AND (?4 IS NULL OR d.question LIKE '%' || ?4 || '%'
                    OR d.answer LIKE '%' || ?4 || '%'
                    OR d.citations LIKE '%' || ?4 || '%')
             ORDER BY d.id DESC"
        ))?;
        let rows = stmt.query_map(
            params![q.repo, q.project, q.initiative, q.grep],
            decision_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Records `forge stats --reprice`'s run (docs/ECONOMIST.md,
    /// "Repricing a free-reporting provider"): a decision row like any
    /// other (docs/SUPERVISOR.md, "Every answer is a decision row"), but
    /// with no `task_id` — the run touches attempts across many tasks, or
    /// none, so it names no single one.
    pub fn insert_reprice_decision(&self, question: &str, answer: &str) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO decisions (task_id, repo, question, answer, created_at, answered_by, citations, answered_for)
             VALUES (NULL, '', ?1, ?2, ?3, 'operator', '', NULL)",
            params![question, answer, crate::unix_now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record a reference on a task: the pull request it landed as, the
    /// issue it came from.
    pub fn insert_task_ref(
        &self,
        task_id: i64,
        kind: &str,
        url: &str,
        label: &str,
        by: &str,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO task_refs (task_id, kind, url, label, by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![task_id, kind, url, label, by, crate::unix_now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// A task's references, oldest first.
    pub fn task_refs(&self, task_id: i64) -> Result<Vec<TaskRef>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM task_refs WHERE task_id = ?1 ORDER BY id",
            TASK_REF_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![task_id], task_ref_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_answers_in_lineage_counts_sibling_retries_too() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let t = Task {
            repo: "r".into(),
            task: "t".into(),
            base_branch: "main".into(),
            model: "m".into(),
            max_turns: 1,
            max_attempts: 2,
            timeout_secs: 1,
            ..Default::default()
        };
        let root = s.insert_task(&t).unwrap();
        let mut retry = t.clone();
        retry.retry_of = Some(root);
        let a = s.insert_task(&retry).unwrap();
        let b = s.insert_task(&retry).unwrap();
        s.insert_decision_by(a, "r", "q", "a", "supervisor", "", None)
            .unwrap();
        assert_eq!(s.supervisor_answers_in_lineage(b).unwrap(), 1);
    }

    #[test]
    fn plugin_enabled_flag_reads_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        assert!(s.enabled_plugins().unwrap().is_empty(), "never recorded");
        s.set_plugin_enabled("notify", true, 100).unwrap();
        assert_eq!(
            s.enabled_plugins().unwrap(),
            ["notify".to_string()].into_iter().collect()
        );
        s.set_plugin_enabled("notify", false, 200).unwrap();
        assert!(s.enabled_plugins().unwrap().is_empty());
    }
}
