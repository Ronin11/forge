//! Intake acceptance: turning a confirmed brief into a project (see
//! docs/INTAKE.md, "Build order", item 3). `forge intake accept` parses
//! its arguments and prints; this module holds the kernel logic, so the
//! interview-to-project transition is not something only the CLI can do.

use crate::ctx::Forge;
use crate::report::Event;
use crate::store::{DeployTarget, Project, Task};
use crate::unix_now;
use crate::view::{Brief, BriefWorkflow, workflow_paragraph};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};

/// Deploy methods `intake accept` can draft a target for without operator
/// help: the built-in action files under `src/builtins/operations/deploy-*.toml`.
const SUPPORTED_DEPLOY_METHODS: [&str; 4] = [
    "deploy-command",
    "deploy-user-service",
    "deploy-static",
    "deploy-pipeline",
];

/// A `(host, method)` pair to draft a deploy target from "where it runs",
/// only when that text names both a host Forge can already reach without
/// more setup (today, just `local`) and one of the built-in methods by
/// name; otherwise `None`, and the caller files a backlog entry instead
/// (see docs/DEPLOY.md, "A target").
fn resolve_draft_deploy(where_it_runs: &str) -> Option<(String, String)> {
    let lower = where_it_runs.to_lowercase();
    let method = SUPPORTED_DEPLOY_METHODS
        .iter()
        .find(|m| lower.contains(*m))?;
    let names_local = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == "local");
    names_local.then(|| ("local".to_string(), method.to_string()))
}

/// Parse a task's recorded plan as a confirmed brief: refuses a task that
/// did not run the intake workflow, has no plan yet, has a plan that is
/// not a brief, is not yet confirmed, or names no workflows.
fn parse_confirmed_brief(t: &Task) -> Result<Brief> {
    if t.workflow != "intake" {
        bail!("task {} did not run the intake workflow", t.id);
    }
    if t.plan.is_empty() {
        bail!(
            "task {} has no recorded brief (the interview has not written one yet)",
            t.id
        );
    }
    let brief: Brief = serde_json::from_str(&t.plan)
        .with_context(|| format!("task {}'s plan is not a brief: {}", t.id, t.plan))?;
    if !brief.confirmed {
        bail!("task {}'s brief has not been confirmed yet", t.id);
    }
    if brief.workflows.is_empty() {
        bail!("task {}'s brief names no workflows", t.id);
    }
    Ok(brief)
}

/// Each of the brief's workflows, in order, paired with the paragraph
/// `accept` would file for it and whether that exact text is already in
/// the project's backlog (so re-accepting an already-accepted brief never
/// duplicates an entry).
fn partition_backlog<'a>(
    workflows: &'a [BriefWorkflow],
    existing: &BTreeSet<String>,
) -> Vec<(&'a BriefWorkflow, String, bool)> {
    workflows
        .iter()
        .map(|w| {
            let text = workflow_paragraph(w);
            let already_filed = existing.contains(&text);
            (w, text, already_filed)
        })
        .collect()
}

/// One workflow's backlog outcome.
pub enum BacklogOutcome {
    Filed { id: i64, workflow: String },
    AlreadyFiled { workflow: String },
}

/// What became of the draft deploy target.
pub enum DeployOutcome {
    TargetAdded,
    TargetAlreadyExists,
    BacklogFiled { id: i64, where_it_runs: String },
    BacklogAlreadyFiled,
}

/// What `accept` did, in enough detail for a caller to report it: the
/// project's name and whether it was just created, the repository
/// registered (if any), each workflow's backlog outcome, and the draft
/// deploy target's outcome.
pub struct Accepted {
    pub project: String,
    pub project_created: bool,
    pub repo: Option<String>,
    pub backlog: Vec<BacklogOutcome>,
    pub deploy: DeployOutcome,
}

impl Accepted {
    /// What the verb prints, one line per fact: the project, the
    /// repository (if one was registered), each workflow's backlog
    /// outcome, then the draft deploy target's outcome.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![if self.project_created {
            format!("created project {}", self.project)
        } else {
            format!("project {} already exists", self.project)
        }];
        if let Some(repo) = &self.repo {
            out.push(format!(
                "registered repository {repo} to project {}",
                self.project
            ));
        }
        for b in &self.backlog {
            out.push(match b {
                BacklogOutcome::Filed { id, workflow } => {
                    format!("added backlog item {id}: {workflow}")
                }
                BacklogOutcome::AlreadyFiled { workflow } => {
                    format!("backlog item for {workflow} already exists")
                }
            });
        }
        out.push(match &self.deploy {
            DeployOutcome::TargetAdded => format!(
                "added draft deploy target draft to project {} (finish it with `forge project deploy set`)",
                self.project
            ),
            DeployOutcome::TargetAlreadyExists => {
                format!("draft deploy target already exists for project {}", self.project)
            }
            DeployOutcome::BacklogFiled { id, where_it_runs } => {
                format!("added backlog item {id}: deploy target ({where_it_runs})")
            }
            DeployOutcome::BacklogAlreadyFiled => {
                "backlog item for deploy target already exists".to_string()
            }
        });
        out
    }
}

/// Accept a confirmed intake task's brief: create (or reuse) the project,
/// fill its backlog with one entry per workflow the brief named, and
/// record a draft deploy target from "where it runs" when it names a host
/// and method Forge already supports (else a backlog entry saying what the
/// target would be). Refused unless the task's brief says `confirmed`.
/// `project` overrides the default project name (the interviewed person's
/// name, slugged); `repo`, when given, is registered to the project and
/// used for the draft deploy target instead of the intake task's own
/// repository.
pub fn accept(
    f: &Forge,
    task_id: i64,
    project: Option<String>,
    repo: Option<String>,
) -> Result<Accepted> {
    let t = f
        .store
        .task(task_id)?
        .with_context(|| format!("no task {task_id}"))?;
    let brief = parse_confirmed_brief(&t)?;

    let person = f
        .store
        .decisions_in_lineage(task_id)?
        .into_iter()
        .rev()
        .map(|d| d.answered_for.unwrap_or(d.answered_by))
        .next()
        .unwrap_or_else(|| "person".to_string());
    let project_name = project.unwrap_or_else(|| crate::engine::slug(&person));

    let project_created = f.store.project(&project_name)?.is_none();
    if project_created {
        f.store.create_project(&Project {
            name: project_name.clone(),
            purpose: workflow_paragraph(&brief.workflows[0]),
            created_at: unix_now(),
            ..Default::default()
        })?;
        f.report.emit(
            task_id,
            Event::ProjectCreated {
                project: &project_name,
                person: &person,
            },
        );
    }

    if let Some(repo) = &repo {
        f.store.register_repo(&project_name, repo, None)?;
    }

    let existing_backlog: BTreeSet<String> = f
        .store
        .backlog(&project_name)?
        .into_iter()
        .map(|item| item.text)
        .collect();

    let mut backlog = Vec::new();
    for (w, text, already_filed) in partition_backlog(&brief.workflows, &existing_backlog) {
        if already_filed {
            backlog.push(BacklogOutcome::AlreadyFiled {
                workflow: w.name.clone(),
            });
            continue;
        }
        let id = f.store.add_backlog(&project_name, &text)?;
        backlog.push(BacklogOutcome::Filed {
            id,
            workflow: w.name.clone(),
        });
    }

    let deploy = match resolve_draft_deploy(&brief.where_it_runs) {
        Some((host, method)) => {
            if f.store.deploy_target(&project_name, "draft")?.is_some() {
                DeployOutcome::TargetAlreadyExists
            } else {
                let mut args = BTreeMap::new();
                args.insert("host".to_string(), host);
                f.store.add_deploy_target(&DeployTarget {
                    project: project_name.clone(),
                    name: "draft".to_string(),
                    repo: repo.clone().unwrap_or_else(|| t.repo.clone()),
                    scope: None,
                    method,
                    args,
                    check_cmd: String::new(),
                    on_landing: false,
                    smoke_url: None,
                })?;
                DeployOutcome::TargetAdded
            }
        }
        None => {
            let text = format!(
                "deploy target: {} (names no host and method Forge already supports; complete with `forge project deploy add`)",
                brief.where_it_runs
            );
            if existing_backlog.contains(&text) {
                DeployOutcome::BacklogAlreadyFiled
            } else {
                let id = f.store.add_backlog(&project_name, &text)?;
                DeployOutcome::BacklogFiled {
                    id,
                    where_it_runs: brief.where_it_runs.clone(),
                }
            }
        }
    };

    Ok(Accepted {
        project: project_name,
        project_created,
        repo,
        backlog,
        deploy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIEF_JSON: &str = r#"{
        "workflows": [
            {"name": "quote by photo", "trigger": "a photo comes in", "inputs": "a photo",
             "outputs": "a quote", "other_people": "none", "failure_today": "nothing",
             "success_signal": "a quote sent", "do_not_touch": "billing"},
            {"name": "weekly invoice", "trigger": "friday", "inputs": "jobs done",
             "outputs": "an invoice", "other_people": "none", "failure_today": "nothing",
             "success_signal": "invoice sent", "do_not_touch": "billing"}
        ],
        "where_it_runs": "local, deploy-command",
        "confirmed": true
    }"#;

    fn confirmed_task() -> Task {
        Task {
            id: 1,
            workflow: "intake".to_string(),
            plan: BRIEF_JSON.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn parse_confirmed_brief_reads_workflows_and_where_it_runs() {
        let brief = parse_confirmed_brief(&confirmed_task()).unwrap();
        assert_eq!(brief.workflows.len(), 2);
        assert_eq!(brief.workflows[0].name, "quote by photo");
        assert_eq!(brief.where_it_runs, "local, deploy-command");
    }

    #[test]
    fn parse_confirmed_brief_refuses_the_wrong_workflow() {
        let mut t = confirmed_task();
        t.workflow = "direct".to_string();
        let err = parse_confirmed_brief(&t).unwrap_err().to_string();
        assert!(err.contains("did not run the intake workflow"), "{err}");
    }

    #[test]
    fn parse_confirmed_brief_refuses_an_empty_plan() {
        let mut t = confirmed_task();
        t.plan = String::new();
        let err = parse_confirmed_brief(&t).unwrap_err().to_string();
        assert!(err.contains("no recorded brief"), "{err}");
    }

    #[test]
    fn parse_confirmed_brief_refuses_an_unconfirmed_brief() {
        let mut t = confirmed_task();
        t.plan = BRIEF_JSON.replace("\"confirmed\": true", "\"confirmed\": false");
        let err = parse_confirmed_brief(&t).unwrap_err().to_string();
        assert!(err.contains("has not been confirmed"), "{err}");
    }

    #[test]
    fn resolve_draft_deploy_matches_local_and_a_known_method() {
        assert_eq!(
            resolve_draft_deploy("local, deploy-command"),
            Some(("local".to_string(), "deploy-command".to_string()))
        );
        assert_eq!(resolve_draft_deploy("a customer's own server"), None);
        assert_eq!(resolve_draft_deploy("deploy-command, but remote"), None);
    }

    #[test]
    fn partition_backlog_skips_a_paragraph_already_in_the_project() {
        let brief = parse_confirmed_brief(&confirmed_task()).unwrap();
        let first_text = workflow_paragraph(&brief.workflows[0]);
        let existing: BTreeSet<String> = [first_text.clone()].into();

        let plan = partition_backlog(&brief.workflows, &existing);
        assert_eq!(plan.len(), 2);
        assert!(plan[0].2, "the first workflow's paragraph is already filed");
        assert_eq!(plan[0].1, first_text);
        assert!(
            !plan[1].2,
            "the second workflow's paragraph is not yet filed"
        );
    }

    #[test]
    fn partition_backlog_files_everything_against_an_empty_backlog() {
        let brief = parse_confirmed_brief(&confirmed_task()).unwrap();
        let plan = partition_backlog(&brief.workflows, &BTreeSet::new());
        assert_eq!(plan.len(), 2);
        assert!(plan.iter().all(|(_, _, already_filed)| !already_filed));
    }

    fn fixture_forge() -> (tempfile::TempDir, Forge) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::ctx::Paths {
            worktrees: dir.path().join("worktrees"),
            logs: dir.path().join("logs"),
            home: dir.path().to_path_buf(),
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = crate::store::Store::open(&dir.path().join("forge.db")).unwrap();
        let f = Forge::open_with(paths, store).unwrap();
        (dir, f)
    }

    #[test]
    fn accept_creates_the_project_and_files_the_backlog_and_draft_target() {
        let (_dir, f) = fixture_forge();
        let mut t = Task {
            repo: "/tmp/nate-shop".to_string(),
            workflow: "intake".to_string(),
            ..Default::default()
        };
        t.id = f.store.insert_task(&t).unwrap();
        t.plan = BRIEF_JSON.to_string();
        f.store.update_task(&t).unwrap();

        let accepted = accept(&f, t.id, None, None).unwrap();
        assert_eq!(accepted.project, "person");
        assert!(accepted.project_created);
        assert_eq!(accepted.backlog.len(), 2);
        assert!(matches!(accepted.deploy, DeployOutcome::TargetAdded));

        assert!(f.store.project("person").unwrap().is_some());
        let backlog = f.store.backlog("person").unwrap();
        assert_eq!(backlog.len(), 2);
        let target = f.store.deploy_target("person", "draft").unwrap();
        assert!(target.is_some());

        // Accepting again is a no-op: no duplicate backlog or project.
        let accepted_again = accept(&f, t.id, None, None).unwrap();
        assert!(!accepted_again.project_created);
        assert!(
            accepted_again
                .backlog
                .iter()
                .all(|b| matches!(b, BacklogOutcome::AlreadyFiled { .. }))
        );
        assert!(matches!(
            accepted_again.deploy,
            DeployOutcome::TargetAlreadyExists
        ));
        assert_eq!(f.store.backlog("person").unwrap().len(), 2);
    }

    #[test]
    fn accept_refuses_an_unconfirmed_brief() {
        let (_dir, f) = fixture_forge();
        let mut t = Task {
            repo: "/tmp/nate-shop".to_string(),
            workflow: "intake".to_string(),
            ..Default::default()
        };
        t.id = f.store.insert_task(&t).unwrap();
        t.plan = BRIEF_JSON.replace("\"confirmed\": true", "\"confirmed\": false");
        f.store.update_task(&t).unwrap();
        assert!(accept(&f, t.id, None, None).is_err());
    }
}
