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
/// sandbox, the reporter).
pub struct Spec<'a> {
    /// The task or job the run's events carry.
    pub id: i64,
    pub step: &'a str,
    pub dir: &'a Path,
    /// Registered repository; None for scratch-only directives.
    pub identity_repo: Option<&'a Path>,
    pub prompt: &'a str,
    /// System-level content for a runner with its own system channel
    /// (`Runner::Chat`); every other runner ignores it. Empty when the
    /// caller has none (see `agent::Launch::system`).
    pub system: &'a str,
    pub model: &'a str,
    pub max_turns: u32,
    pub timeout: Duration,
    /// How long the checks after this run may take; zero when none follow.
    pub check_timeout: Duration,
    pub log_path: &'a Path,
    pub provider: &'a agent::Provider,
    pub schema: &'a str,
    /// Whether the run goes through the operator's sandbox. A job's
    /// directive step never does: it has no tools. The deploy look does,
    /// in an empty scratch directory with the smoke output bound read-only.
    pub sandboxed: bool,
    /// What a sandboxed run may reach besides its provider's model
    /// endpoints; ignored unsandboxed.
    pub egress: Egress<'a>,
    pub writes: bool,
    pub start_sha: &'a str,
    pub resume: Option<&'a str>,
    /// See `agent::Launch::budget_usd`.
    pub budget_usd: Option<f64>,
    pub no_tools: bool,
    /// What a `Runner::Jev` judges; see `agent::Launch::judgment`.
    pub judgment: Option<agent::Judgment<'a>>,
    /// The `Watch` thresholds: `Forge::effective_early_ending` for a
    /// task's attempt, the operator's `[early_ending]` config otherwise.
    pub early_ending: crate::config::EarlyEnding,
}

/// Besides its provider's model endpoints, what a sandboxed run's
/// directory may reach.
pub enum Egress<'a> {
    /// A task's directory: what its repository's trusted config declares,
    /// as far as the task's trust level lets it (see `Forge::allow_egress`).
    Project {
        cfg: &'a crate::config::Config,
        trust: crate::store::Trust,
    },
    /// `Project` with the config `task_egress` read for it.
    Loaded {
        cfg: Box<crate::config::Config>,
        trust: crate::store::Trust,
    },
    /// A scratch directory no repository owns: nothing else.
    ModelOnly,
}

/// `Egress::Project` for task `t`'s worktree `wt`, its repository's config
/// read at `rev`: what a run there may reach, as every other run in that
/// worktree may.
pub async fn task_egress(t: &crate::store::Task, wt: &Path, rev: &str) -> Result<Egress<'static>> {
    let cfg = crate::config::load_at(Path::new(&t.repo), wt, rev).await?;
    Ok(Egress::Loaded {
        cfg: Box::new(cfg),
        trust: t.trust,
    })
}

/// Register what a sandboxed run in `s.dir` may reach, with `s.provider`'s
/// own model endpoints, so its proxy never depends on what an earlier run
/// in the same directory left behind: the supervisor's run in a blocked
/// task's worktree was refused its model endpoint when it did (2026-10-03).
fn register_egress(f: &Forge, s: &Spec<'_>) {
    if !s.sandboxed {
        return;
    }
    match &s.egress {
        Egress::Project { cfg, trust } => f.allow_egress_for(s.dir, cfg, *trust, Some(s.provider)),
        Egress::Loaded { cfg, trust } => f.allow_egress_for(s.dir, cfg, *trust, Some(s.provider)),
        Egress::ModelOnly => f.allow_model_egress(s.dir, s.provider),
    }
}

pub async fn launch(f: &Forge, s: Spec<'_>) -> Result<Outcome> {
    register_egress(f, &s);
    let kernel;
    let identity_repo = match s.identity_repo {
        Some(repo) => repo,
        None => {
            kernel = crate::git::kernel_repository(&f.paths.home, s.dir).await?;
            &kernel
        }
    };
    let identity = crate::git::identity(identity_repo).await;
    agent::run(agent::Launch {
        task_id: s.id,
        worktree: s.dir,
        identity,
        prompt: s.prompt,
        system: s.system,
        model: s.model,
        max_turns: s.max_turns,
        timeout: s.timeout,
        check_timeout: s.check_timeout,
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
        budget_usd: s.budget_usd,
        writes: s.writes,
        start_sha: s.start_sha,
        schema: s.schema,
        early_ending: s.early_ending,
        no_tools: s.no_tools,
        judgment: s.judgment,
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
    /// The provider refused the agent's login (expired, revoked): a
    /// refusal like a spent window, held until a person logs in again.
    LoginRefused,
    /// Forge ended the run itself on signs it was going nowhere.
    EndedEarly(String),
    TimedOut,
    /// The CLI stopped the run at the task's remaining budget
    /// (`--max-budget-usd`): the task is capped, as the worker caps it.
    BudgetCapped,
    /// Work finished without the final structured report.
    StructuredOutput,
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
    if a.login_refused {
        Some(Failure::LoginRefused)
    } else if a.rate_limited {
        Some(Failure::RateLimited)
    } else if let Some(why) = &a.ended_early {
        Some(Failure::EndedEarly(why.clone()))
    } else if a.timed_out {
        Some(Failure::TimedOut)
    } else if a.budget_hit {
        Some(Failure::BudgetCapped)
    } else if a.terminal_reason.as_deref() == Some("structured_output_retry_exhausted")
        || a.subtype.as_deref() == Some("error_max_structured_output_retries")
    {
        Some(Failure::StructuredOutput)
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
            Failure::LoginRefused => "the provider refused the agent login".into(),
            Failure::EndedEarly(why) => format!("stopped early: {why}"),
            Failure::TimedOut => "agent timed out".into(),
            Failure::BudgetCapped => "stopped at the task's budget cap".into(),
            Failure::StructuredOutput => {
                "structured_output_retry_exhausted: envelope missing".into()
            }
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
            Failure::RateLimited | Failure::LoginRefused | Failure::EndedEarly(_) => self.reason(),
            Failure::Error { subtype, .. } => quote(format!(
                "agent result {:?}",
                subtype.as_deref().unwrap_or("error")
            )),
            Failure::TimedOut
            | Failure::BudgetCapped
            | Failure::StructuredOutput
            | Failure::Exit(_)
            | Failure::NoResult => quote(self.reason()),
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

    fn sandboxed_forge(home: &Path) -> Forge {
        let paths = crate::ctx::Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home: home.to_path_buf(),
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = crate::store::Store::open(&paths.home.join("forge.db")).unwrap();
        let mut f = Forge::open_with(paths, store).unwrap();
        let sandbox =
            crate::sandbox::Sandbox::with_bwrap("/bin/false".into(), home.join("sandbox-home"));
        f.sandbox = Some(crate::executor::Execution::bwrap_only(sandbox));
        f
    }

    fn spec<'a>(
        f: &Forge,
        dir: &'a Path,
        provider: &'a agent::Provider,
        sandboxed: bool,
        egress: Egress<'a>,
    ) -> Spec<'a> {
        Spec {
            id: 1,
            step: "supervisor",
            dir,
            identity_repo: None,
            prompt: "",
            system: "",
            model: "opus",
            max_turns: 1,
            timeout: Duration::from_secs(1),
            check_timeout: Duration::ZERO,
            log_path: dir,
            provider,
            schema: "",
            sandboxed,
            egress,
            writes: false,
            start_sha: "",
            resume: None,
            no_tools: false,
            judgment: None,
            early_ending: f.early_ending,
        }
    }

    fn hosts(f: &Forge, dir: &Path) -> Vec<String> {
        f.sandbox
            .as_ref()
            .unwrap()
            .policy_for(dir)
            .rules()
            .iter()
            .map(|r| r.to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_sandboxed_launch_registers_its_own_providers_hosts_for_its_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let f = sandboxed_forge(&tmp.path().join("home"));
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("forge.toml"),
            "[defaults]\nbase_branch = \"main\"\n[sandbox]\negress = [\"registry.npmjs.org\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_working(&repo).await.unwrap();
        let claude = agent::Provider::default();
        // Nothing registered the worktree before: no earlier attempt ran in
        // this process, as for a supervisor reading a blocked task.
        assert!(hosts(&f, &repo).is_empty());
        register_egress(
            &f,
            &spec(
                &f,
                &repo,
                &claude,
                true,
                Egress::Project {
                    cfg: &cfg,
                    trust: crate::store::Trust::Operator,
                },
            ),
        );
        let got = hosts(&f, &repo);
        assert!(got.contains(&"*.anthropic.com".to_string()), "{got:?}");
        assert!(got.contains(&"registry.npmjs.org".to_string()), "{got:?}");

        // The launch's own provider replaces whatever an earlier run with
        // another provider left for the same directory.
        let codex = agent::Provider {
            name: "codex".into(),
            runner: agent::Runner::CodexCli,
            ..Default::default()
        };
        f.allow_egress_for(&repo, &cfg, crate::store::Trust::Operator, Some(&codex));
        assert!(!hosts(&f, &repo).contains(&"*.anthropic.com".to_string()));
        register_egress(
            &f,
            &spec(
                &f,
                &repo,
                &claude,
                true,
                Egress::Project {
                    cfg: &cfg,
                    trust: crate::store::Trust::Operator,
                },
            ),
        );
        let got = hosts(&f, &repo);
        assert!(got.contains(&"*.anthropic.com".to_string()), "{got:?}");
        assert!(!got.contains(&"*.openai.com".to_string()), "{got:?}");

        // A scratch directory reaches the model endpoint alone.
        let scratch = tmp.path().join("scratch");
        register_egress(&f, &spec(&f, &scratch, &claude, true, Egress::ModelOnly));
        let got = hosts(&f, &scratch);
        assert!(got.contains(&"*.anthropic.com".to_string()), "{got:?}");
        assert!(!got.contains(&"registry.npmjs.org".to_string()), "{got:?}");

        // An unsandboxed launch registers nothing.
        let host = tmp.path().join("host");
        register_egress(&f, &spec(&f, &host, &claude, false, Egress::ModelOnly));
        assert!(hosts(&f, &host).is_empty());
    }

    #[tokio::test]
    async fn launch_registers_egress_before_entering_the_runner() {
        let tmp = tempfile::tempdir().unwrap();
        let f = sandboxed_forge(&tmp.path().join("home"));
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("forge.toml"),
            "[defaults]\nbase_branch = \"main\"\n[sandbox]\negress = [\"registry.npmjs.org\"]\n",
        )
        .unwrap();
        let cfg = crate::config::load_working(&repo).await.unwrap();
        let provider = agent::Provider {
            name: "judge".into(),
            runner: agent::Runner::CodexCli,
            base_url: Some("https://judge.example/v1".into()),
            ..Default::default()
        };
        // Public tasks get only model hosts, even when the directory's
        // previous operator run could reach the project's declared hosts.
        f.allow_egress_for(
            &repo,
            &cfg,
            crate::store::Trust::Operator,
            Some(&agent::Provider::default()),
        );
        let mut s = spec(
            &f,
            &repo,
            &provider,
            true,
            Egress::Project {
                cfg: &cfg,
                trust: crate::store::Trust::Public,
            },
        );
        s.identity_repo = Some(&repo);
        // Codex rejects tool-free runs before spawning a CLI. This exercises
        // launch itself without requiring a real agent or network access.
        s.no_tools = true;
        let err = launch(&f, s).await.unwrap_err();
        assert!(err.to_string().contains("cannot yet guarantee no tool use"));
        let got = hosts(&f, &repo);
        assert!(got.contains(&"judge.example".to_string()), "{got:?}");
        assert!(!got.contains(&"*.anthropic.com".to_string()), "{got:?}");
        assert!(!got.contains(&"registry.npmjs.org".to_string()), "{got:?}");
    }

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
