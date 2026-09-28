//! Adopted tasks: a branch a human made outside Forge, verified and landed
//! by `forge adopt` through the integrator with no agent run (see
//! docs/OPS.md, "Landing hand-made work"). The record is a task like any
//! other, marked by `origin`; the outcome statistics and the economist's
//! data measure agent workflows, so they leave these rows out and
//! `forge stats` counts them apart as manual work.

use super::*;
use serde::{Deserialize, Serialize};

/// Where a task's work came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Origin {
    /// An agent coded it under a workflow: every task but an adopted one.
    #[default]
    Agent,
    /// A human wrote it; `forge adopt` only verified and landed it.
    Adopted,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Agent => "agent",
            Origin::Adopted => "adopted",
        }
    }

    /// No agent wrote this task's commits: the manual marker `forge
    /// log`, `forge show`, the portal and `forge stats` print.
    pub fn is_manual(self) -> bool {
        self == Origin::Adopted
    }
}

impl TryFrom<&str> for Origin {
    type Error = std::io::Error;
    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        Ok(match s {
            "agent" => Origin::Agent,
            "adopted" => Origin::Adopted,
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown task origin {other:?}"
                )));
            }
        })
    }
}

/// What `forge adopt` took in: the branch as the human named it (empty
/// when they named a bare commit), the commit it resolved to and was
/// verified at, and who adopted it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adoption {
    pub branch: String,
    pub commit: String,
    pub by: String,
}

impl Adoption {
    /// The branch when one was named, else the commit: what a human
    /// called the work.
    pub fn source(&self) -> &str {
        if self.branch.is_empty() {
            &self.commit
        } else {
            &self.branch
        }
    }

    /// One line for `forge show` and the portal.
    pub fn describe(&self) -> String {
        let commit = &self.commit[..self.commit.len().min(8)];
        let by = if self.by.is_empty() {
            String::new()
        } else {
            format!(" by {}", self.by)
        };
        if self.branch.is_empty() {
            format!("adopted {commit}{by} (manual: no agent ran)")
        } else {
            format!(
                "adopted {} @ {commit}{by} (manual: no agent ran)",
                self.branch
            )
        }
    }
}

pub(super) fn to_column(a: Option<&Adoption>) -> Result<String> {
    Ok(match a {
        Some(a) => serde_json::to_string(a)?,
        None => String::new(),
    })
}

pub(super) fn from_column(s: &str) -> Option<Adoption> {
    if s.is_empty() {
        None
    } else {
        serde_json::from_str(s).ok()
    }
}

/// Adopted tasks in `forge stats`: counted apart from every workflow's
/// outcomes, since no agent's work is being measured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ManualStat {
    pub tasks: i64,
    pub landed: i64,
    /// Verified and left for a human (`--no-land`), not landed yet.
    pub verified: i64,
    pub blocked: i64,
    pub failed: i64,
}

impl Store {
    /// Put an envelope on an attempt the kernel wrote: how an adopted
    /// task's question reaches `forge requests` and `forge show`, which
    /// read it from the task's last attempt.
    pub fn set_attempt_envelope(&self, id: i64, envelope_json: &str) -> Result<()> {
        self.lock().retry_execute(
            "UPDATE attempts SET envelope_json=?2 WHERE id=?1",
            params![id, envelope_json],
        )?;
        Ok(())
    }

    /// The adopted tasks within `scope`, by outcome.
    pub fn manual_stats(&self, scope: &StatsFilter) -> Result<ManualStat> {
        Ok(self.lock().retry_query_row(
            "SELECT COUNT(*) AS tasks, COALESCE(SUM(landed_sha != ''), 0) AS landed,
                    COALESCE(SUM(state = 'succeeded' AND landed_sha = ''), 0) AS verified,
                    COALESCE(SUM(state = 'blocked'), 0) AS blocked,
                    COALESCE(SUM(state = 'failed'), 0) AS failed
             FROM tasks WHERE origin = 'adopted'
               AND (?1 IS NULL OR project = ?1) AND (?2 IS NULL OR initiative = ?2)",
            params![scope.project, scope.initiative],
            |r| {
                Ok(ManualStat {
                    tasks: r.get("tasks")?,
                    landed: r.get("landed")?,
                    verified: r.get("verified")?,
                    blocked: r.get("blocked")?,
                    failed: r.get("failed")?,
                })
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("forge.db")).unwrap();
        (dir, store)
    }

    fn adopted() -> Task {
        Task {
            repo: "/r".into(),
            task: "Adopt branch jetpack".into(),
            title: Some("Jetpack yields to wall".into()),
            base_branch: "master".into(),
            workflow: "adopt".into(),
            origin: Origin::Adopted,
            adoption: Some(Adoption {
                branch: "jetpack".into(),
                commit: "008f5f4".repeat(5) + "00000",
                by: "nate".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn an_adopted_task_round_trips_its_origin_and_adoption() {
        let (_d, s) = open();
        let id = s.insert_task(&adopted()).unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!(t.origin, Origin::Adopted);
        assert!(t.origin.is_manual());
        let a = t.adoption.unwrap();
        assert_eq!(a.branch, "jetpack");
        assert_eq!(a.by, "nate");
        assert_eq!(t.title.as_deref(), Some("Jetpack yields to wall"));
    }

    #[test]
    fn an_agent_task_has_no_adoption_and_is_not_manual() {
        let (_d, s) = open();
        let id = s
            .insert_task(&Task {
                repo: "/r".into(),
                task: "write 42".into(),
                ..Default::default()
            })
            .unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!(t.origin, Origin::Agent);
        assert!(!t.origin.is_manual());
        assert!(t.adoption.is_none());
    }

    #[test]
    fn an_unknown_origin_is_refused() {
        assert!(Origin::try_from("agent").is_ok());
        assert!(Origin::try_from("adopted").is_ok());
        assert!(Origin::try_from("human").is_err());
    }

    #[test]
    fn describe_names_the_branch_or_the_bare_commit() {
        let a = Adoption {
            branch: "jetpack".into(),
            commit: "008f5f4aabbccdd".into(),
            by: "nate".into(),
        };
        assert_eq!(
            a.describe(),
            "adopted jetpack @ 008f5f4a by nate (manual: no agent ran)"
        );
        assert_eq!(a.source(), "jetpack");
        let bare = Adoption {
            branch: String::new(),
            ..a
        };
        assert_eq!(
            bare.describe(),
            "adopted 008f5f4a by nate (manual: no agent ran)"
        );
        assert_eq!(bare.source(), "008f5f4aabbccdd");
    }

    #[test]
    fn a_queued_adopted_task_is_never_offered_to_a_worker() {
        let (_d, s) = open();
        let id = s
            .insert_task(&Task {
                state: TaskState::Queued,
                ..adopted()
            })
            .unwrap();
        assert!(s.queued_unblocked(&[]).unwrap().is_empty());
        assert!(s.claim_next(1, &[], |_| false).unwrap().is_none());
        assert_eq!(s.task(id).unwrap().unwrap().state, TaskState::Queued);
    }

    #[test]
    fn manual_stats_count_only_adopted_tasks() {
        let (_d, s) = open();
        s.insert_task(&Task {
            state: TaskState::Succeeded,
            ..adopted()
        })
        .unwrap();
        let landed = s
            .insert_task(&Task {
                state: TaskState::Succeeded,
                ..adopted()
            })
            .unwrap();
        let mut t = s.task(landed).unwrap().unwrap();
        t.landed_sha = "abc".into();
        s.update_task(&t).unwrap();
        s.insert_task(&Task {
            state: TaskState::Blocked,
            ..adopted()
        })
        .unwrap();
        s.insert_task(&Task {
            state: TaskState::Succeeded,
            ..Default::default()
        })
        .unwrap();
        let m = s.manual_stats(&StatsFilter::default()).unwrap();
        assert_eq!(
            m,
            ManualStat {
                tasks: 3,
                landed: 1,
                verified: 1,
                blocked: 1,
                failed: 0,
            }
        );
    }
}
