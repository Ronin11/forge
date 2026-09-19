//! One launcher for every bounded agent run, and one reading of how a run
//! failed. Five places launch an agent — an attempt's step, a job's
//! directive step, the assessment after a landing, the look at a deployed
//! page, the supervisor — and before this module each filled the same
//! launch from `Forge` its own way and diagnosed the outcome with its own
//! strings (docs/REVIEW-2.md, theme 2.2). What differs between them is the
//! `Spec`; what is the same lives here.

use crate::agent::{self, Outcome};
use crate::ctx::Forge;
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

/// What one launch needs beyond what every launch takes from `Forge` (the
/// sandbox, the reporter, the early-ending thresholds).
pub struct Spec<'a> {
    /// The task or job the run's events carry.
    pub id: i64,
    pub step: &'a str,
    pub dir: &'a Path,
    pub prompt: &'a str,
    /// System-level content for a runner with its own system channel
    /// (`Runner::Chat`); every other runner ignores it. Empty when the
    /// caller has none (see `agent::Launch::system`).
    pub system: &'a str,
    pub model: &'a str,
    pub max_turns: u32,
    pub timeout: Duration,
    pub log_path: &'a Path,
    pub provider: &'a agent::Provider,
    pub schema: &'a str,
    /// Whether the run goes through the operator's sandbox. A job's
    /// directive step and the deploy look never do: the first has no
    /// tools, the second reads a screenshot from a scratch directory,
    /// and `forge deploy` never sandboxes its own steps.
    pub sandboxed: bool,
    pub writes: bool,
    pub start_sha: &'a str,
    pub resume: Option<&'a str>,
    pub no_tools: bool,
}

pub async fn launch(f: &Forge, s: Spec<'_>) -> Result<Outcome> {
    agent::run(agent::Launch {
        task_id: s.id,
        worktree: s.dir,
        prompt: s.prompt,
        system: s.system,
        model: s.model,
        max_turns: s.max_turns,
        timeout: s.timeout,
        log_path: s.log_path,
        sandbox: if s.sandboxed {
            f.sandbox.as_ref()
        } else {
            None
        },
        report: &f.report,
        step: s.step,
        provider: s.provider,
        resume: s.resume,
        writes: s.writes,
        start_sha: s.start_sha,
        schema: s.schema,
        early_ending: f.early_ending,
        no_tools: s.no_tools,
    })
    .await
}

/// The structured result a run produced, parsed as `T`; an error when
/// there was none or it does not fit.
pub fn structured<T: serde::de::DeserializeOwned>(o: &Outcome) -> Result<T> {
    o.structured
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .context("no structured result fit the schema")
}

/// How a run itself failed, when it did: the one reading every launcher
/// shares. `reason` renders it as the record's strings; `tail` as a job
/// step's diagnosis.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The provider refused the run for a rate window: not the agent's fault.
    RateLimited,
    /// Forge ended the run itself on signs it was going nowhere.
    EndedEarly(String),
    TimedOut,
    /// The CLI's result frame reported an error, with its own subtype
    /// (`error_max_turns`, `error_during_execution`, ...) and the exit.
    Error {
        subtype: Option<String>,
        exit: Option<i32>,
    },
    /// A non-zero exit (or a signal) without an error result.
    Exit(Option<i32>),
    /// Exit 0 and no result frame at all.
    NoResult,
}

pub fn failure(a: &Outcome) -> Option<Failure> {
    if a.rate_limited {
        Some(Failure::RateLimited)
    } else if let Some(why) = &a.ended_early {
        Some(Failure::EndedEarly(why.clone()))
    } else if a.timed_out {
        Some(Failure::TimedOut)
    } else if a.got_result && a.is_error {
        Some(Failure::Error {
            subtype: a.subtype.clone(),
            exit: a.exit_code,
        })
    } else if a.exit_code != Some(0) {
        Some(Failure::Exit(a.exit_code))
    } else if !a.got_result {
        Some(Failure::NoResult)
    } else {
        None
    }
}

fn exit_text(code: Option<i32>) -> String {
    format!(
        "agent exit {}",
        code.map_or("signal".into(), |c| c.to_string())
    )
}

impl Failure {
    /// The record's wording: what an attempt's reason, `audit::diagnose`
    /// and the supervisor's escalation say. A non-zero exit is named even
    /// when the result frame carried an error subtype, since the exit is
    /// what the worktree left behind explains.
    pub fn reason(&self) -> String {
        match self {
            Failure::RateLimited => "rate limited by the provider".into(),
            Failure::EndedEarly(why) => format!("stopped early: {why}"),
            Failure::TimedOut => "agent timed out".into(),
            Failure::Error {
                exit: Some(code), ..
            } if *code != 0 => exit_text(Some(*code)),
            Failure::Error { .. } => "agent reported an error".into(),
            Failure::Exit(code) => exit_text(*code),
            Failure::NoResult => "agent produced no result".into(),
        }
    }

    /// A job step's wording: the result frame's own subtype and the last
    /// lines of stderr, never the bare exit code, since a directive step
    /// has no worktree left behind to inspect; the log and this tail are
    /// the only diagnosis it leaves (docs/JOBS.md, "Steps").
    pub fn tail(&self, stderr_tail: &str) -> String {
        let quote = |reason: String| {
            if stderr_tail.is_empty() {
                reason
            } else {
                format!("{reason}\nstderr:\n{stderr_tail}")
            }
        };
        match self {
            Failure::RateLimited | Failure::EndedEarly(_) => self.reason(),
            Failure::Error { subtype, .. } => quote(format!(
                "agent result {:?}",
                subtype.as_deref().unwrap_or("error")
            )),
            Failure::TimedOut | Failure::Exit(_) | Failure::NoResult => quote(self.reason()),
        }
    }
}

/// Why the agent run itself counts as failed, if it does, in the record's
/// wording.
pub fn agent_failure(a: &Outcome) -> Option<String> {
    failure(a).map(|f| f.reason())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_failure_reasons() {
        let ok = Outcome {
            exit_code: Some(0),
            got_result: true,
            ..Default::default()
        };
        assert_eq!(agent_failure(&ok), None);
        assert_eq!(
            agent_failure(&Outcome {
                timed_out: true,
                ..Default::default()
            })
            .as_deref(),
            Some("agent timed out")
        );
        assert_eq!(
            agent_failure(&Outcome {
                exit_code: Some(2),
                ..Default::default()
            })
            .as_deref(),
            Some("agent exit 2")
        );
        assert_eq!(
            agent_failure(&Outcome {
                exit_code: Some(0),
                ..Default::default()
            })
            .as_deref(),
            Some("agent produced no result")
        );
        assert_eq!(
            agent_failure(&Outcome {
                exit_code: Some(0),
                got_result: true,
                is_error: true,
                ..Default::default()
            })
            .as_deref(),
            Some("agent reported an error")
        );
        // An error result with a non-zero exit is named by its exit on the
        // record (the capped-commit path), by its subtype in a job's tail.
        let capped = Outcome {
            exit_code: Some(1),
            got_result: true,
            is_error: true,
            subtype: Some("error_max_turns".into()),
            ..Default::default()
        };
        assert_eq!(agent_failure(&capped).as_deref(), Some("agent exit 1"));
        assert_eq!(
            failure(&capped).unwrap().tail("boom"),
            "agent result \"error_max_turns\"\nstderr:\nboom"
        );
    }

    #[test]
    fn a_job_tail_never_names_the_bare_exit_code_for_an_error_result() {
        let o = Outcome {
            exit_code: Some(1),
            got_result: true,
            is_error: true,
            subtype: Some("error_during_execution".into()),
            ..Default::default()
        };
        let tail = failure(&o).unwrap().tail("");
        assert_eq!(tail, "agent result \"error_during_execution\"");
        assert!(!tail.contains("agent exit"));
        // Without a result frame the exit is all there is, quoted with stderr.
        let crashed = Outcome {
            exit_code: Some(1),
            ..Default::default()
        };
        assert_eq!(
            failure(&crashed).unwrap().tail("segv"),
            "agent exit 1\nstderr:\nsegv"
        );
        // Rate limits and early endings carry no stderr: not the agent's output.
        assert_eq!(
            failure(&Outcome {
                rate_limited: true,
                ..Default::default()
            })
            .unwrap()
            .tail("noise"),
            "rate limited by the provider"
        );
    }

    #[test]
    fn structured_parses_or_names_the_gap() {
        #[derive(serde::Deserialize, Debug)]
        struct R {
            score: i64,
        }
        let o = Outcome {
            structured: Some(r#"{"score": 7}"#.into()),
            ..Default::default()
        };
        assert_eq!(structured::<R>(&o).unwrap().score, 7);
        let none = Outcome::default();
        assert_eq!(
            structured::<R>(&none).unwrap_err().to_string(),
            "no structured result fit the schema"
        );
        let bad = Outcome {
            structured: Some(r#"{"score": "seven"}"#.into()),
            ..Default::default()
        };
        assert!(structured::<R>(&bad).is_err());
    }
}
