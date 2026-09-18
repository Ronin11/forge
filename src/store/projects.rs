use super::*;

pub(super) const PROJECT_COLUMNS: &[&str] = &[
    "name",
    "purpose",
    "created_at",
    "workflow",
    "per_task_usd",
    "per_initiative_usd",
    "supervisor_model",
    "supervisor_per_lineage",
    "protected_json",
    "role_providers_json",
];

pub(super) const PROJECT_REPO_COLUMNS: &[&str] = &["project", "repo", "scope_json"];

pub(super) const BACKLOG_COLUMNS: &[&str] = &["id", "project", "text", "created_at", "done_at"];

pub(super) const INITIATIVE_COLUMNS: &[&str] = &[
    "id",
    "project",
    "outcome",
    "budget_usd",
    "stop_after_same_rule",
    "created_at",
    "settled_at",
];

fn project_from_row(r: &Row) -> rusqlite::Result<Project> {
    let protected_json: Option<String> = r.get("protected_json")?;
    let role_providers_json: Option<String> = r.get("role_providers_json")?;
    Ok(Project {
        name: r.get("name")?,
        purpose: r.get("purpose")?,
        created_at: r.get("created_at")?,
        workflow: r.get("workflow")?,
        per_task_usd: r.get("per_task_usd")?,
        per_initiative_usd: r.get("per_initiative_usd")?,
        supervisor_model: r.get("supervisor_model")?,
        supervisor_per_lineage: r.get("supervisor_per_lineage")?,
        protected: protected_json.map(|j| serde_json::from_str(&j).unwrap_or_default()),
        role_providers: role_providers_json
            .map(|j| serde_json::from_str(&j).unwrap_or_default())
            .unwrap_or_default(),
    })
}

fn project_repo_from_row(r: &Row) -> rusqlite::Result<ProjectRepo> {
    Ok(ProjectRepo {
        repo: r.get("repo")?,
        scope: r.get("scope_json")?,
    })
}

fn backlog_from_row(r: &Row) -> rusqlite::Result<BacklogItem> {
    Ok(BacklogItem {
        id: r.get("id")?,
        project: r.get("project")?,
        text: r.get("text")?,
        created_at: r.get("created_at")?,
        done_at: r.get("done_at")?,
    })
}

fn initiative_from_row(r: &Row) -> rusqlite::Result<Initiative> {
    Ok(Initiative {
        id: r.get("id")?,
        project: r.get("project")?,
        outcome: r.get("outcome")?,
        budget_usd: r.get("budget_usd")?,
        stop_after_same_rule: r.get("stop_after_same_rule")?,
        created_at: r.get("created_at")?,
        settled_at: r.get("settled_at")?,
    })
}

impl Store {
    /// Register a new project. Fails if the name is already taken.
    pub fn create_project(&self, p: &Project) -> Result<()> {
        self.lock().execute(
            "INSERT INTO projects (name, purpose, created_at) VALUES (?1, ?2, ?3)",
            params![p.name, p.purpose, p.created_at],
        )?;
        Ok(())
    }

    pub fn project(&self, name: &str) -> Result<Option<Project>> {
        Ok(self
            .lock()
            .query_row(
                &format!(
                    "SELECT {} FROM projects WHERE name=?1",
                    PROJECT_COLUMNS.join(", ")
                ),
                params![name],
                project_from_row,
            )
            .optional()?)
    }

    /// Every project, alphabetically.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM projects ORDER BY name",
            PROJECT_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Apply `forge project set`'s changes: only the columns given (not
    /// `None`) change; `role_providers` merges into the existing map
    /// instead of replacing it, so setting one role leaves the others
    /// alone. Returns `false` if no project has this name.
    pub fn set_project_defaults(&self, name: &str, d: &ProjectDefaults) -> Result<bool> {
        let protected = d
            .protected
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let role_providers = if d.role_providers.is_empty() {
            None
        } else {
            let current: Option<String> = self
                .lock()
                .query_row(
                    "SELECT role_providers_json FROM projects WHERE name=?1",
                    params![name],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let mut merged: BTreeMap<String, String> = current
                .as_deref()
                .map(|j| serde_json::from_str(j).unwrap_or_default())
                .unwrap_or_default();
            merged.extend(d.role_providers.clone());
            Some(serde_json::to_string(&merged)?)
        };
        let n = self.lock().execute(
            "UPDATE projects SET
                purpose = COALESCE(?2, purpose),
                workflow = COALESCE(?3, workflow),
                per_task_usd = COALESCE(?4, per_task_usd),
                per_initiative_usd = COALESCE(?5, per_initiative_usd),
                supervisor_model = COALESCE(?6, supervisor_model),
                supervisor_per_lineage = COALESCE(?7, supervisor_per_lineage),
                protected_json = COALESCE(?8, protected_json),
                role_providers_json = COALESCE(?9, role_providers_json)
             WHERE name=?1",
            params![
                name,
                d.purpose,
                d.workflow,
                d.per_task_usd,
                d.per_initiative_usd,
                d.supervisor_model,
                d.supervisor_per_lineage,
                protected,
                role_providers,
            ],
        )?;
        Ok(n > 0)
    }

    /// Every project that lists `repo`, alphabetically: the names an
    /// ambiguous-repository refusal names.
    pub fn projects_listing_repo(&self, repo: &str) -> Result<Vec<String>> {
        let c = self.lock();
        let mut stmt =
            c.prepare("SELECT project FROM project_repos WHERE repo=?1 ORDER BY project")?;
        let rows = stmt.query_map(params![repo], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Add a backlog item to a project. Returns its id.
    pub fn add_backlog(&self, project: &str, text: &str) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO backlog (project, text, created_at) VALUES (?1, ?2, ?3)",
            params![project, text, crate::unix_now()],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// A project's backlog, oldest first.
    pub fn backlog(&self, project: &str) -> Result<Vec<BacklogItem>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM backlog WHERE project=?1 ORDER BY id",
            BACKLOG_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], backlog_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mark a backlog item done. `false` if it does not exist in this
    /// project or is already done.
    pub fn mark_backlog_done(&self, project: &str, id: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE backlog SET done_at=?3 WHERE id=?1 AND project=?2 AND done_at IS NULL",
            params![id, project, crate::unix_now()],
        )?;
        Ok(n > 0)
    }

    /// Mint a fresh portal token for a project (see docs/PORTAL.md, "What
    /// it is"): `forge project portal` generates the token text itself
    /// (32 random bytes, hex-encoded) and records it here.
    pub fn create_portal_token(&self, project: &str, token: &str, at: i64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO portal_tokens (token, project, created_at) VALUES (?1, ?2, ?3)",
            params![token, project, at],
        )?;
        Ok(())
    }

    /// Revoke every currently-active token on a project (`forge project
    /// portal --revoke`). Returns how many were revoked.
    pub fn revoke_portal_tokens(&self, project: &str, at: i64) -> Result<usize> {
        Ok(self.lock().execute(
            "UPDATE portal_tokens SET revoked_at=?2 WHERE project=?1 AND revoked_at IS NULL",
            params![project, at],
        )?)
    }

    /// The project an active (unrevoked) portal token opens, if any: how
    /// `forge project resolve-token` resolves `/p/<token>` for the portal
    /// server (see docs/PORTAL.md).
    pub fn portal_token_project(&self, token: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT project FROM portal_tokens WHERE token=?1 AND revoked_at IS NULL",
                params![token],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// List a repository under a project, with an optional scope (the
    /// paths within it the project owns; `None` means the whole
    /// repository). Registering the same pair again replaces the scope.
    pub fn register_repo(&self, project: &str, repo: &str, scope: Option<&str>) -> Result<()> {
        self.lock().execute(
            "INSERT INTO project_repos (project, repo, scope_json) VALUES (?1, ?2, ?3)
             ON CONFLICT(project, repo) DO UPDATE SET scope_json = excluded.scope_json",
            params![project, repo, scope],
        )?;
        Ok(())
    }

    /// The project's first repository, in the order it was registered
    /// (`forge project new --repo` lists it first, or a lone `forge
    /// project new ... --repo` call the only one): what an initiative's
    /// `--from` file falls to for a paragraph with no `repo:` line.
    pub fn first_repo(&self, project: &str) -> Result<Option<String>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT repo FROM project_repos WHERE project=?1 ORDER BY rowid LIMIT 1",
                params![project],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// A project's repositories, alphabetically.
    pub fn project_repos(&self, project: &str) -> Result<Vec<ProjectRepo>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM project_repos WHERE project=?1 ORDER BY repo",
            PROJECT_REPO_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], project_repo_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The project that lists `repo`, when exactly one does; `None` if no
    /// project lists it, or more than one does.
    pub fn default_project_for_repo(&self, repo: &str) -> Result<Option<String>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT project FROM project_repos WHERE repo=?1")?;
        let names: Vec<String> = stmt
            .query_map(params![repo], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(match names.len() {
            1 => names.into_iter().next(),
            _ => None,
        })
    }

    /// The default project for `repo`, creating one if the repository is
    /// not yet listed by any project: named by the repository's base
    /// name, or `forge` for the Forge repository itself, the same rule
    /// the migration applies to pre-existing tasks. `None` when the
    /// repository is already listed by more than one project (ambiguous;
    /// `queue::enqueue` turns that into a refusal naming them, since only
    /// the operator can say which with `forge add --project`).
    pub fn ensure_default_project(&self, repo: &str) -> Result<Option<String>> {
        if let Some(name) = self.default_project_for_repo(repo)? {
            return Ok(Some(name));
        }
        let ambiguous: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM project_repos WHERE repo=?1",
            params![repo],
            |r| r.get(0),
        )?;
        if ambiguous > 0 {
            return Ok(None);
        }
        let name = project_name_for_repo(repo);
        if self.project(&name)?.is_none() {
            self.create_project(&Project {
                name: name.clone(),
                purpose: format!("Repository {repo}."),
                created_at: crate::unix_now(),
                ..Default::default()
            })?;
        }
        self.register_repo(&name, repo, None)?;
        Ok(Some(name))
    }

    /// Register a new initiative. Returns its id.
    pub fn create_initiative(&self, ini: &Initiative) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO initiatives (project, outcome, budget_usd, stop_after_same_rule, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                ini.project,
                ini.outcome,
                ini.budget_usd,
                ini.stop_after_same_rule,
                ini.created_at
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Change only the fields `d` gives; returns `false` if `id` names no
    /// initiative.
    pub fn set_initiative(&self, id: i64, d: &InitiativeUpdate) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE initiatives SET
                outcome = COALESCE(?2, outcome),
                budget_usd = COALESCE(?3, budget_usd),
                stop_after_same_rule = COALESCE(?4, stop_after_same_rule)
             WHERE id=?1",
            params![id, d.outcome, d.budget_usd, d.stop_after_same_rule],
        )?;
        Ok(n > 0)
    }

    pub fn initiative(&self, id: i64) -> Result<Option<Initiative>> {
        Ok(self
            .lock()
            .query_row(
                &format!(
                    "SELECT {} FROM initiatives WHERE id=?1",
                    INITIATIVE_COLUMNS.join(", ")
                ),
                params![id],
                initiative_from_row,
            )
            .optional()?)
    }

    /// Every initiative, oldest first; only `project`'s when given.
    pub fn list_initiatives(&self, project: Option<&str>) -> Result<Vec<Initiative>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM initiatives WHERE ?1 IS NULL OR project = ?1 ORDER BY id",
            INITIATIVE_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], initiative_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// An initiative's tasks, oldest first.
    pub fn initiative_tasks(&self, id: i64) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE initiative=?1 ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![id], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A project's tasks, oldest first.
    pub fn project_tasks(&self, project: &str) -> Result<Vec<Task>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM tasks WHERE project=?1 ORDER BY id",
            TASK_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The summed cost of every attempt of every one of an initiative's tasks.
    pub fn initiative_cost(&self, id: i64) -> Result<f64> {
        Ok(self.lock().query_row(
            "SELECT COALESCE(SUM(a.cost_usd), 0) FROM attempts a
             WHERE a.task_id IN (SELECT id FROM tasks WHERE initiative=?1)",
            params![id],
            |r| r.get(0),
        )?)
    }

    /// Every initiative id with at least one queued task: the only ones
    /// worth checking for a hold before the worker claims (see
    /// `view::initiative_hold`).
    pub fn initiatives_with_queued_tasks(&self) -> Result<Vec<i64>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT DISTINCT initiative FROM tasks WHERE state='queued' AND initiative IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Mark an initiative settled now. `false` if it already was.
    pub fn settle_initiative(&self, id: i64, at: i64) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE initiatives SET settled_at=?2 WHERE id=?1 AND settled_at IS NULL",
            params![id, at],
        )?;
        Ok(n > 0)
    }

    /// Task counts by state and total cost for one project.
    pub fn project_task_stats(&self, project: &str) -> Result<ProjectTaskStats> {
        let c = self.lock();
        Ok(c.query_row(
            "SELECT SUM(state='queued') AS queued, SUM(state='running') AS running,
                    SUM(state='succeeded') AS succeeded, SUM(state='failed') AS failed,
                    SUM(state='unverified') AS unverified, SUM(state='blocked') AS blocked,
                    SUM(state='withdrawn') AS withdrawn,
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN
                        (SELECT id FROM tasks WHERE project=?1)), 0) AS cost
             FROM tasks WHERE project=?1",
            params![project],
            |r| {
                Ok(ProjectTaskStats {
                    queued: r.get::<_, Option<i64>>("queued")?.unwrap_or(0),
                    running: r.get::<_, Option<i64>>("running")?.unwrap_or(0),
                    succeeded: r.get::<_, Option<i64>>("succeeded")?.unwrap_or(0),
                    failed: r.get::<_, Option<i64>>("failed")?.unwrap_or(0),
                    unverified: r.get::<_, Option<i64>>("unverified")?.unwrap_or(0),
                    blocked: r.get::<_, Option<i64>>("blocked")?.unwrap_or(0),
                    withdrawn: r.get::<_, Option<i64>>("withdrawn")?.unwrap_or(0),
                    cost: r.get("cost")?,
                })
            },
        )?)
    }

    /// Tasks, landed count, cost and defect escape per project: what
    /// `forge stats` adds below the per-workflow table when it is not
    /// itself scoped to one project or initiative (see docs/PROJECTS.md,
    /// "The record, scoped").
    pub fn project_stats(&self) -> Result<Vec<ProjectStat>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT t.project AS project, COUNT(*) AS tasks, SUM(t.landed_sha != '') AS landed,
                    COALESCE((SELECT SUM(a.cost_usd) FROM attempts a WHERE a.task_id IN (SELECT id FROM tasks t2 WHERE t2.project=t.project)), 0) AS cost,
                    SUM(t.landed_sha != '' AND EXISTS (
                        SELECT 1 FROM attempts a
                        JOIN tasks b ON b.id = a.task_id
                        WHERE b.base_sha = t.landed_sha
                          AND a.step = 'code'
                          AND a.attempt_no = (SELECT MIN(a2.attempt_no) FROM attempts a2 WHERE a2.task_id = a.task_id AND a2.step = 'code')
                          AND EXISTS (
                              SELECT 1 FROM json_each(a.verdict_json) j
                              WHERE json_extract(j.value, '$.level') = 'L1' AND json_extract(j.value, '$.ok') = 0
                          )
                    )) AS broke_base
             FROM tasks t WHERE t.project IS NOT NULL AND t.state IN ('succeeded','failed','blocked','unverified') AND t.started_at IS NOT NULL
             GROUP BY t.project ORDER BY t.project",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectStat {
                project: r.get("project")?,
                tasks: r.get("tasks")?,
                landed: r.get("landed")?,
                cost: r.get("cost")?,
                broke_base: r.get("broke_base")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_for_repo_is_none_unless_exactly_one_project_lists_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        assert_eq!(s.default_project_for_repo("/r").unwrap(), None);

        s.create_project(&Project {
            name: "a".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        s.register_repo("a", "/r", None).unwrap();
        assert_eq!(
            s.default_project_for_repo("/r").unwrap(),
            Some("a".to_string())
        );

        s.create_project(&Project {
            name: "b".into(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        s.register_repo("b", "/r", None).unwrap();
        assert_eq!(
            s.default_project_for_repo("/r").unwrap(),
            None,
            "listed by two projects now"
        );
    }

    #[test]
    fn ensure_default_project_creates_one_the_first_time_a_repo_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let repo = dir.path().join("myrepo").display().to_string();
        assert_eq!(
            s.ensure_default_project(&repo).unwrap(),
            Some("myrepo".to_string())
        );
        // Idempotent: the same project is reused, not duplicated.
        assert_eq!(
            s.ensure_default_project(&repo).unwrap(),
            Some("myrepo".to_string())
        );
        assert_eq!(s.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn set_project_defaults_role_providers_merges_instead_of_replacing() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.create_project(&Project {
            name: "p".into(),
            purpose: "purpose".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                role_providers: [("code".to_string(), "devhome".to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                role_providers: [("review".to_string(), "openai".to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let p = s.project("p").unwrap().unwrap();
        assert_eq!(p.role_providers["code"], "devhome", "not clobbered");
        assert_eq!(p.role_providers["review"], "openai");
        assert_eq!(p.role_providers.len(), 2);

        // Naming the same role again overwrites just that entry.
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                role_providers: [("code".to_string(), "openai".to_string())].into(),
                ..Default::default()
            },
        )
        .unwrap();
        let p = s.project("p").unwrap().unwrap();
        assert_eq!(p.role_providers["code"], "openai");
        assert_eq!(p.role_providers["review"], "openai");
    }

    #[test]
    fn set_project_defaults_purpose_replaces_the_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.create_project(&Project {
            name: "p".into(),
            purpose: "Repository /home/x/repo.".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
        assert!(is_placeholder_purpose(
            &s.project("p").unwrap().unwrap().purpose
        ));

        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                purpose: Some("What this project is for.".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let p = s.project("p").unwrap().unwrap();
        assert_eq!(p.purpose, "What this project is for.");
        assert!(!is_placeholder_purpose(&p.purpose));

        // A `None` purpose (no `--purpose` given) leaves it alone.
        s.set_project_defaults(
            "p",
            &ProjectDefaults {
                workflow: Some("other".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            s.project("p").unwrap().unwrap().purpose,
            "What this project is for."
        );
    }

    #[test]
    fn ensure_default_project_gives_a_new_project_the_placeholder_purpose() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let repo = dir.path().join("myrepo").display().to_string();
        s.ensure_default_project(&repo).unwrap();
        let p = s.project("myrepo").unwrap().unwrap();
        assert!(is_placeholder_purpose(&p.purpose), "{}", p.purpose);
    }

    fn mk_project(s: &Store, name: &str) {
        s.create_project(&Project {
            name: name.to_string(),
            purpose: "p".into(),
            created_at: 1,
            ..Default::default()
        })
        .unwrap();
    }

    #[test]
    fn a_minted_portal_token_resolves_to_its_project_and_a_stranger_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.portal_token_project("nope").unwrap().is_none());

        s.create_portal_token("equitizr", "tok1", 1).unwrap();
        assert_eq!(
            s.portal_token_project("tok1").unwrap().as_deref(),
            Some("equitizr")
        );
    }

    #[test]
    fn revoking_a_projects_tokens_stops_them_resolving_but_leaves_other_projects_alone() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        mk_project(&s, "nucleosynthesis");

        s.create_portal_token("equitizr", "tok1", 1).unwrap();
        s.create_portal_token("equitizr", "tok2", 2).unwrap();
        s.create_portal_token("nucleosynthesis", "tok3", 3).unwrap();

        let n = s.revoke_portal_tokens("equitizr", 10).unwrap();
        assert_eq!(n, 2, "both of equitizr's tokens were active");

        assert!(s.portal_token_project("tok1").unwrap().is_none());
        assert!(s.portal_token_project("tok2").unwrap().is_none());
        assert_eq!(
            s.portal_token_project("tok3").unwrap().as_deref(),
            Some("nucleosynthesis"),
            "revoking one project's tokens must not touch another's"
        );

        // Revoking again finds nothing left active.
        assert_eq!(s.revoke_portal_tokens("equitizr", 20).unwrap(), 0);
    }

    #[test]
    fn a_project_can_carry_more_than_one_active_token_until_revoked() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        s.create_portal_token("equitizr", "tok1", 1).unwrap();
        s.create_portal_token("equitizr", "tok2", 2).unwrap();
        assert_eq!(
            s.portal_token_project("tok1").unwrap().as_deref(),
            Some("equitizr"),
            "minting a second token does not itself revoke the first"
        );
        assert_eq!(
            s.portal_token_project("tok2").unwrap().as_deref(),
            Some("equitizr")
        );
    }
}
