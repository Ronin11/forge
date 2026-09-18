use super::*;
use serde::Serialize;

/// A deploy target: where a project's landed code runs, how it gets
/// there, and what proves it is up (see docs/DEPLOY.md, "A target").
/// `scope` is the raw JSON array of paths within `repo` the target
/// deploys, `None` for the whole repository, mirroring `ProjectRepo`.
#[derive(Debug, Clone, Serialize)]
pub struct DeployTarget {
    pub project: String,
    pub name: String,
    pub repo: String,
    pub scope: Option<String>,
    /// The action file this target runs, e.g. "deploy-command".
    pub method: String,
    pub args: BTreeMap<String, String>,
    pub check_cmd: String,
    pub on_landing: bool,
    /// A url the deploy-smoke operation opens in headless Chromium after
    /// the check passes, `None` to skip the smoke step entirely (see
    /// docs/DEPLOY.md, "A deterministic smoke step").
    pub smoke_url: Option<String>,
}

/// One deploy: a target, the commit deployed, when it started and
/// finished, the check's verdict and output, and what it rolled back to
/// if the check failed (see docs/DEPLOY.md, "When a deploy runs").
#[derive(Debug, Clone, Serialize)]
pub struct Deploy {
    pub id: i64,
    pub project: String,
    pub target: String,
    pub sha: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub check_ok: Option<bool>,
    pub check_output: String,
    pub rolled_back_to: Option<String>,
    pub reason: String,
    /// The task this deploy ran on behalf of, when it was an on-landing
    /// target rather than an operator-invoked `forge deploy`. Recorded now;
    /// not part of `--json` output until a later step needs it there.
    #[allow(dead_code)]
    #[serde(skip)]
    pub task_id: Option<i64>,
    /// Whether the deploy-smoke operation passed, `None` when the target
    /// declares no smoke url or the check never passed for smoke to run.
    pub smoke_ok: Option<bool>,
    /// The smoke operation's own record: console errors, failed requests,
    /// title and screenshot path, as the JSON it wrote (see
    /// src/builtins/operations/deploy-smoke.toml).
    pub smoke_json: Option<String>,
    /// Whether the `deploy-look` directive found the deployed page fit to
    /// show anyone, `None` when the target declared no smoke url or the
    /// screenshot smoke took was never produced for it to look at (see
    /// src/deploy_look.rs).
    pub look_ok: Option<bool>,
    /// `deploy-look`'s findings, as the JSON `[{"severity":"blocking"|
    /// "notable","finding":...}]` it returned.
    pub look_json: Option<String>,
}

/// One run of the assess directive against a landed task (see
/// src/assess.rs): a maintainability score 0-10 and a list of findings, as
/// the JSON `[{"path":...,"finding":...,"severity":"notable"|"concern"}]`
/// the directive returned, with what ran it and what it cost. Surfaced by
/// `forge show`, `forge trace --json` and `forge initiative report`;
/// never by `forge stats`.
#[derive(Debug, Clone)]
pub struct Assessment {
    /// Recorded now; no view needs it (each reads a task's single most
    /// recent assessment by `task_id`, not this row's own id).
    #[allow(dead_code)]
    pub id: i64,
    pub task_id: i64,
    pub score: i64,
    pub findings_json: String,
    pub model: String,
    pub provider: String,
    pub cost_usd: Option<f64>,
    pub created_at: i64,
}

pub(super) const DEPLOY_TARGET_COLUMNS: &[&str] = &[
    "project",
    "name",
    "repo",
    "scope_json",
    "method",
    "args_json",
    "check_cmd",
    "on_landing",
    "smoke_url",
];

pub(super) const DEPLOY_COLUMNS: &[&str] = &[
    "id",
    "project",
    "target",
    "sha",
    "started_at",
    "finished_at",
    "check_ok",
    "check_output",
    "rolled_back_to",
    "reason",
    "task_id",
    "smoke_ok",
    "smoke_json",
    "look_ok",
    "look_json",
];

pub(super) const ASSESSMENT_COLUMNS: &[&str] = &[
    "id",
    "task_id",
    "score",
    "findings_json",
    "model",
    "provider",
    "cost_usd",
    "created_at",
];

fn deploy_target_from_row(r: &Row) -> rusqlite::Result<DeployTarget> {
    Ok(DeployTarget {
        project: r.get("project")?,
        name: r.get("name")?,
        repo: r.get("repo")?,
        scope: r.get("scope_json")?,
        method: r.get("method")?,
        args: serde_json::from_str(&r.get::<_, String>("args_json")?).unwrap_or_default(),
        check_cmd: r.get("check_cmd")?,
        on_landing: r.get("on_landing")?,
        smoke_url: r.get("smoke_url")?,
    })
}

fn deploy_from_row(r: &Row) -> rusqlite::Result<Deploy> {
    Ok(Deploy {
        id: r.get("id")?,
        project: r.get("project")?,
        target: r.get("target")?,
        sha: r.get("sha")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        check_ok: r.get("check_ok")?,
        check_output: r.get("check_output")?,
        rolled_back_to: r.get("rolled_back_to")?,
        reason: r.get("reason")?,
        task_id: r.get("task_id")?,
        smoke_ok: r.get("smoke_ok")?,
        smoke_json: r.get("smoke_json")?,
        look_ok: r.get("look_ok")?,
        look_json: r.get("look_json")?,
    })
}

fn assessment_from_row(r: &Row) -> rusqlite::Result<Assessment> {
    Ok(Assessment {
        id: r.get("id")?,
        task_id: r.get("task_id")?,
        score: r.get("score")?,
        findings_json: r.get("findings_json")?,
        model: r.get("model")?,
        provider: r.get("provider")?,
        cost_usd: r.get("cost_usd")?,
        created_at: r.get("created_at")?,
    })
}

impl Store {
    /// Declare a deploy target. Fails if `(project, name)` already exists.
    pub fn add_deploy_target(&self, t: &DeployTarget) -> Result<()> {
        let args_json = serde_json::to_string(&t.args)?;
        self.lock().execute(
            "INSERT INTO deploy_targets (project, name, repo, scope_json, method, args_json, check_cmd, on_landing, smoke_url)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                t.project,
                t.name,
                t.repo,
                t.scope,
                t.method,
                args_json,
                t.check_cmd,
                t.on_landing,
                t.smoke_url,
            ],
        )?;
        Ok(())
    }

    /// Replace a deploy target's fields in place (project and name stay
    /// the primary key): what `forge project deploy set` writes after
    /// merging only the flags given onto the row `deploy_target` returned.
    pub fn update_deploy_target(&self, t: &DeployTarget) -> Result<()> {
        let args_json = serde_json::to_string(&t.args)?;
        let n = self.lock().execute(
            "UPDATE deploy_targets SET repo=?3, scope_json=?4, method=?5, args_json=?6, check_cmd=?7, on_landing=?8, smoke_url=?9
             WHERE project=?1 AND name=?2",
            params![
                t.project,
                t.name,
                t.repo,
                t.scope,
                t.method,
                args_json,
                t.check_cmd,
                t.on_landing,
                t.smoke_url,
            ],
        )?;
        if n != 1 {
            bail!("no deploy target {} in project {}", t.name, t.project);
        }
        Ok(())
    }

    /// Delete a deploy target. Its past deploys (`deploys`, `forge deploy
    /// log`) are untouched; only future `--on-landing` runs and `forge
    /// deploy` of this name stop.
    pub fn remove_deploy_target(&self, project: &str, name: &str) -> Result<()> {
        let n = self.lock().execute(
            "DELETE FROM deploy_targets WHERE project=?1 AND name=?2",
            params![project, name],
        )?;
        if n != 1 {
            bail!("no deploy target {name} in project {project}");
        }
        Ok(())
    }

    /// One project's deploy target by name.
    pub fn deploy_target(&self, project: &str, name: &str) -> Result<Option<DeployTarget>> {
        Ok(self
            .lock()
            .query_row(
                &format!(
                    "SELECT {} FROM deploy_targets WHERE project=?1 AND name=?2",
                    DEPLOY_TARGET_COLUMNS.join(", ")
                ),
                params![project, name],
                deploy_target_from_row,
            )
            .optional()?)
    }

    /// A project's deploy targets, alphabetically.
    pub fn deploy_targets(&self, project: &str) -> Result<Vec<DeployTarget>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM deploy_targets WHERE project=?1 ORDER BY name",
            DEPLOY_TARGET_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], deploy_target_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record a deploy starting, optionally tied to the task that landed
    /// and triggered it. Returns its id; `finish_deploy` completes it.
    pub fn start_deploy(
        &self,
        project: &str,
        target: &str,
        sha: &str,
        at: i64,
        task_id: Option<i64>,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO deploys (project, target, sha, started_at, task_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project, target, sha, at, task_id],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Record a deploy's outcome: the check's verdict and output, what it
    /// rolled back to (if it did), why, the smoke operation's verdict when
    /// the target declared a smoke url and the check passed for it to run
    /// (`None`, `None` otherwise), and `deploy-look`'s verdict on the same
    /// terms (`None`, `None` when it never ran).
    #[allow(clippy::too_many_arguments)]
    pub fn finish_deploy(
        &self,
        id: i64,
        at: i64,
        check_ok: bool,
        check_output: &str,
        rolled_back_to: Option<&str>,
        reason: &str,
        smoke_ok: Option<bool>,
        smoke_json: Option<&str>,
        look_ok: Option<bool>,
        look_json: Option<&str>,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE deploys SET finished_at=?2, check_ok=?3, check_output=?4, rolled_back_to=?5, reason=?6, smoke_ok=?7, smoke_json=?8, look_ok=?9, look_json=?10
             WHERE id=?1",
            params![
                id,
                at,
                check_ok,
                check_output,
                rolled_back_to,
                reason,
                smoke_ok,
                smoke_json,
                look_ok,
                look_json
            ],
        )?;
        Ok(())
    }

    /// A project's deploys, newest first; only `target`'s when given: what
    /// `forge deploy log` shows.
    pub fn deploys(&self, project: &str, target: Option<&str>) -> Result<Vec<Deploy>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM deploys WHERE project=?1 AND (?2 IS NULL OR target=?2) ORDER BY id DESC",
            DEPLOY_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project, target], deploy_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// A task's deploys, newest first: the on-landing targets it triggered
    /// when it landed (see docs/DEPLOY.md, "When a deploy runs"). What
    /// `forge show`, `forge trace --json`, and the web task view list
    /// under the task.
    pub fn deploys_for_task(&self, task_id: i64) -> Result<Vec<Deploy>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM deploys WHERE task_id=?1 ORDER BY id DESC",
            DEPLOY_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![task_id], deploy_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record one assess directive run against a landed task. Returns its id.
    pub fn insert_assessment(&self, a: &Assessment) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO assessments (task_id, score, findings_json, model, provider, cost_usd, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                a.task_id,
                a.score,
                a.findings_json,
                a.model,
                a.provider,
                a.cost_usd,
                a.created_at
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// A task's most recent assessment, if the assess directive has ever
    /// run against it (see docs/ACTIONS.md, "Assessment").
    pub fn assessment(&self, task_id: i64) -> Result<Option<Assessment>> {
        Ok(self
            .lock()
            .query_row(
                &format!(
                    "SELECT {} FROM assessments WHERE task_id=?1 ORDER BY id DESC LIMIT 1",
                    ASSESSMENT_COLUMNS.join(", ")
                ),
                params![task_id],
                assessment_from_row,
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn deploy_targets_are_added_and_listed_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.deploy_targets("equitizr").unwrap().is_empty());
        assert!(s.deploy_target("equitizr", "prod").unwrap().is_none());

        let mut args = BTreeMap::new();
        args.insert("unit".to_string(), "equitizr.service".to_string());
        s.add_deploy_target(&DeployTarget {
            project: "equitizr".into(),
            name: "prod".into(),
            repo: "/repo".into(),
            scope: None,
            method: "deploy-user-service".into(),
            args: args.clone(),
            check_cmd: "systemctl is-active equitizr".into(),
            on_landing: true,
            smoke_url: Some("https://equitizr.example.com/".into()),
        })
        .unwrap();
        s.add_deploy_target(&DeployTarget {
            project: "equitizr".into(),
            name: "staging".into(),
            repo: "/repo".into(),
            scope: Some(r#"["web/"]"#.into()),
            method: "deploy-static".into(),
            args: BTreeMap::new(),
            check_cmd: "curl -f https://staging.example.com/health".into(),
            on_landing: false,
            smoke_url: None,
        })
        .unwrap();

        let targets = s.deploy_targets("equitizr").unwrap();
        assert_eq!(targets.len(), 2);
        // Alphabetical: "prod" before "staging".
        assert_eq!(targets[0].name, "prod");
        assert_eq!(targets[0].method, "deploy-user-service");
        assert_eq!(targets[0].args, args);
        assert!(targets[0].on_landing);
        assert_eq!(
            targets[0].smoke_url.as_deref(),
            Some("https://equitizr.example.com/")
        );
        assert_eq!(targets[1].name, "staging");
        assert_eq!(targets[1].scope.as_deref(), Some(r#"["web/"]"#));
        assert!(!targets[1].on_landing);
        assert_eq!(targets[1].smoke_url, None);

        let one = s.deploy_target("equitizr", "prod").unwrap().unwrap();
        assert_eq!(one.check_cmd, "systemctl is-active equitizr");

        // A duplicate (project, name) is refused.
        assert!(
            s.add_deploy_target(&DeployTarget {
                project: "equitizr".into(),
                name: "prod".into(),
                repo: "/repo".into(),
                scope: None,
                method: "deploy-command".into(),
                args: BTreeMap::new(),
                check_cmd: "true".into(),
                on_landing: false,
                smoke_url: None,
            })
            .is_err()
        );
    }

    #[test]
    fn a_deploy_target_is_updated_in_place_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");

        let mut args = BTreeMap::new();
        args.insert("unit".to_string(), "equitizr.service".to_string());
        s.add_deploy_target(&DeployTarget {
            project: "equitizr".into(),
            name: "prod".into(),
            repo: "/repo".into(),
            scope: None,
            method: "deploy-user-service".into(),
            args: args.clone(),
            check_cmd: "systemctl is-active equitizr".into(),
            on_landing: true,
            smoke_url: Some("https://equitizr.example.com/".into()),
        })
        .unwrap();

        // Updating a target that does not exist is refused.
        assert!(
            s.update_deploy_target(&DeployTarget {
                project: "equitizr".into(),
                name: "ghost".into(),
                repo: "/repo".into(),
                scope: None,
                method: "deploy-command".into(),
                args: BTreeMap::new(),
                check_cmd: "true".into(),
                on_landing: false,
                smoke_url: None,
            })
            .is_err()
        );

        // Update in place: the row stays under the same primary key.
        let mut t = s.deploy_target("equitizr", "prod").unwrap().unwrap();
        t.args.insert("host".to_string(), "box2".to_string());
        t.on_landing = false;
        s.update_deploy_target(&t).unwrap();

        let updated = s.deploy_target("equitizr", "prod").unwrap().unwrap();
        assert_eq!(
            updated.args.get("unit").map(String::as_str),
            Some("equitizr.service")
        );
        assert_eq!(updated.args.get("host").map(String::as_str), Some("box2"));
        assert!(!updated.on_landing);
        assert_eq!(updated.check_cmd, "systemctl is-active equitizr");
        assert_eq!(s.deploy_targets("equitizr").unwrap().len(), 1);

        // Removing an unknown target is refused; removing the real one
        // leaves no targets behind.
        assert!(s.remove_deploy_target("equitizr", "ghost").is_err());
        s.remove_deploy_target("equitizr", "prod").unwrap();
        assert!(s.deploy_targets("equitizr").unwrap().is_empty());
        assert!(s.deploy_target("equitizr", "prod").unwrap().is_none());
    }

    #[test]
    fn deploys_are_recorded_and_listed_newest_first_optionally_by_target() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        mk_project(&s, "equitizr");
        assert!(s.deploys("equitizr", None).unwrap().is_empty());

        let a = s
            .start_deploy("equitizr", "prod", "aaaaaaa", 100, None)
            .unwrap();
        let b = s
            .start_deploy("equitizr", "staging", "bbbbbbb", 200, None)
            .unwrap();
        s.finish_deploy(
            a,
            150,
            true,
            "active",
            None,
            "",
            Some(true),
            Some(r#"{"ok":true}"#),
            Some(true),
            Some("[]"),
        )
        .unwrap();
        s.finish_deploy(
            b,
            250,
            false,
            "connection refused",
            Some("aaaaaaa"),
            "the deploy of bbbbbbb failed its check and was rolled back to aaaaaaa",
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let all = s.deploys("equitizr", None).unwrap();
        assert_eq!(all.len(), 2);
        // Newest first.
        assert_eq!(all[0].id, b);
        assert_eq!(all[0].target, "staging");
        assert_eq!(all[0].check_ok, Some(false));
        assert_eq!(all[0].rolled_back_to.as_deref(), Some("aaaaaaa"));
        assert_eq!(all[0].smoke_ok, None);
        assert_eq!(all[1].id, a);
        assert_eq!(all[1].check_ok, Some(true));
        assert_eq!(all[1].finished_at, Some(150));
        assert_eq!(all[1].smoke_ok, Some(true));
        assert_eq!(all[1].smoke_json.as_deref(), Some(r#"{"ok":true}"#));
        assert_eq!(all[0].look_ok, None);
        assert_eq!(all[1].look_ok, Some(true));
        assert_eq!(all[1].look_json.as_deref(), Some("[]"));

        let prod_only = s.deploys("equitizr", Some("prod")).unwrap();
        assert_eq!(prod_only.len(), 1);
        assert_eq!(prod_only[0].id, a);
    }

    /// Pinned against a literal captured while `view::DeployTargetRow` and
    /// `view::DeployRow` still existed: `serde_json::to_string` on each
    /// produced this exact text, so deleting the rows in their favour was a
    /// no-op for every `--json` caller. `Deploy::task_id` is `#[serde(skip)]`
    /// because `DeployRow` never carried it either.
    #[test]
    fn deploy_target_and_deploy_serialize_to_the_captured_row_shape() {
        let target = DeployTarget {
            project: "equitizr".into(),
            name: "prod".into(),
            repo: "equitizr".into(),
            scope: Some(r#"["web"]"#.into()),
            method: "rsync".into(),
            args: BTreeMap::from([("host".to_string(), "example.com".to_string())]),
            check_cmd: "curl -f https://example.com".into(),
            on_landing: true,
            smoke_url: Some("https://example.com".into()),
        };
        let deploy = Deploy {
            id: 9,
            project: "equitizr".into(),
            target: "prod".into(),
            sha: "deadbeef".into(),
            started_at: 100,
            finished_at: Some(140),
            check_ok: Some(true),
            check_output: "ok".into(),
            rolled_back_to: None,
            reason: "".into(),
            task_id: Some(3),
            smoke_ok: Some(true),
            smoke_json: Some("{}".into()),
            look_ok: Some(false),
            look_json: None,
        };

        assert_eq!(
            serde_json::to_string(&target).unwrap(),
            r#"{"project":"equitizr","name":"prod","repo":"equitizr","scope":"[\"web\"]","method":"rsync","args":{"host":"example.com"},"check_cmd":"curl -f https://example.com","on_landing":true,"smoke_url":"https://example.com"}"#,
        );
        assert_eq!(
            serde_json::to_string(&deploy).unwrap(),
            r#"{"id":9,"project":"equitizr","target":"prod","sha":"deadbeef","started_at":100,"finished_at":140,"check_ok":true,"check_output":"ok","rolled_back_to":null,"reason":"","smoke_ok":true,"smoke_json":"{}","look_ok":false,"look_json":null}"#,
        );
    }
}
