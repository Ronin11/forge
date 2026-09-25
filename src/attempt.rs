//! One attempt of a directive: the prompt for the contract, the record
//! of what the agent was given, the launch, the contract's verdict, and
//! the row. Every contract runs through `run_attempt`; what differs is
//! the `Spec` the contract fills in: where the agent works, what it is
//! told, which refs are overlaid, and what the verdict needs.

/// Task and workflow context for one numbered directive attempt.
pub struct RunAttempt<'a> {
    pub f: &'a Forge,
    pub t: &'a Task,
    pub cfg: &'a config::Config,
    pub step: &'a ResolvedStep,
    pub seq: i64,
    pub attempt_no: i64,
    pub feedback: Option<&'a str>,
    pub resume: Option<&'a Resume>,
}

/// Inputs and provider identity recorded when opening an attempt row.
pub struct NewAttempt<'a> {
    pub f: &'a Forge,
    pub t: &'a Task,
    pub step: &'a str,
    pub seq: i64,
    pub dir: &'a Path,
    pub attempt_no: i64,
    pub inputs: Inputs,
    pub resume: Option<&'a Resume>,
    pub provider: &'a agent::Provider,
}

/// Worktree, prompt, and continuation state for launching a task directive.
struct AttemptLaunch<'a> {
    f: &'a Forge,
    t: &'a Task,
    step: &'a str,
    worktree: &'a Path,
    prompt: &'a str,
    log_path: &'a Path,
    resume: Option<&'a str>,
    writes: bool,
    start_sha: &'a str,
    provider: &'a agent::Provider,
}

use crate::audit::{Inputs, Outputs};
use crate::ctx::Forge;
use crate::engine::{Classify, Fault};
use crate::landing::overlay_refs;
use crate::prompts::{
    code_prompt, concierge_prompt, interview_prompt, plan_prompt, review_prompt, tests_prompt,
};
use crate::report::Event;
use crate::store::{Attempt, AttemptState, FinishAttempt, Task};
use crate::verify::{self, Subject, Verdict};
use crate::workflows::{Contract, ResolvedStep};
use crate::{agent, config, git, unix_now};
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The refs whose namespace files are overlaid before L1: the standing
/// suite when the repository has one, the task's own tests when it has
/// some.
/// A capped attempt to continue: the CLI session, and where that attempt
/// started, since the agent reports for the whole session.
#[derive(Clone, Debug)]
pub struct Resume {
    pub session: String,
    pub start_sha: String,
    /// The `fresh` arm of the continuation factor: the capped attempt's
    /// log, from which a handoff is built for a new session. `None` resumes
    /// the session.
    pub fresh_from: Option<PathBuf>,
}

/// What one contract's attempt differs in.
struct Spec {
    /// Where the agent works: the task's clone, or the tests step's own.
    dir: PathBuf,
    prompt: String,
    inputs: Inputs,
    /// Refs whose namespace files are overlaid before L1 (code only).
    overlay_refs: Vec<String>,
    /// The ref the attempt's commits are recorded under (tests only).
    verify_ref: Option<String>,
    /// The tests contract's scratch directory for red-on-base.
    scratch: Option<PathBuf>,
}

/// Run one attempt of `step` for the task: build the contract's spec,
/// open the attempt row, launch the agent, judge the result, record it.
pub async fn run_attempt(
    args: RunAttempt<'_>,
) -> Result<(Attempt, Verdict, agent::Outcome), Fault> {
    let RunAttempt {
        f,
        t,
        cfg,
        step,
        seq,
        attempt_no,
        feedback,
        resume,
    } = args;
    let contract = step.action.contract;
    let repo = Path::new(&t.repo);
    // The initiative's outcome, when this task belongs to one: placed in
    // every step's prompt as "Why this task exists" (see
    // docs/PROJECTS.md, "Initiative").
    let outcome = t
        .initiative
        .and_then(|id| f.store.initiative(id).ok().flatten())
        .map(|i| i.outcome);
    let journal = if t.journal && contract != Contract::Review {
        let j = crate::journal::journal_for(f, t)?;
        (!j.is_empty()).then_some(j)
    } else {
        None
    };
    let context = (t.context_enabled && !t.context.is_empty()).then(|| t.context.clone());
    let common = Inputs {
        feedback: feedback.map(str::to_string),
        task_checks: t.checks.clone(),
        protected: cfg.protected.clone(),
        namespace: cfg.namespace.clone(),
        resumed: resume
            .filter(|r| r.fresh_from.is_none())
            .map(|r| r.session.clone()),
        continuation: resume.map(|r| {
            if r.fresh_from.is_some() {
                "fresh"
            } else {
                "resume"
            }
            .to_string()
        }),
        journal: journal.clone(),
        context: context.clone(),
        ..Default::default()
    };
    let spec = match contract {
        Contract::Code => {
            let refs = overlay_refs(repo, t.id, Some(&t.verify_base)).await;
            Spec {
                dir: PathBuf::from(&t.worktree),
                prompt: code_prompt(crate::prompts::DirectivePrompt {
                    t,
                    cfg,
                    step,
                    n: attempt_no,
                    feedback,
                    journal: journal.as_deref(),
                    outcome: outcome.as_deref(),
                }),
                inputs: Inputs {
                    interface: (!t.interface.is_empty()).then(|| t.interface.clone()),
                    plan: (!t.plan.is_empty()).then(|| t.plan.clone()),
                    overlay_refs: refs.clone(),
                    checks_shown: t.show_checks,
                    ..common
                },
                overlay_refs: refs,
                verify_ref: None,
                scratch: None,
            }
        }
        Contract::Tests => {
            // The tests step's own clone of the base, apart from the coder's.
            let dir = tests_clone_dir(&t.worktree);
            if !dir.exists() {
                let base_ref = cfg
                    .push_remote
                    .as_ref()
                    .map(|n| format!("refs/remotes/{n}/{}", t.base_branch));
                git::clone_task(
                    repo,
                    &t.base_branch,
                    &dir,
                    &format!("verify/{}", t.id),
                    base_ref.as_deref(),
                    Some(&t.base_sha),
                )
                .await
                .env()?;
            }
            Spec {
                dir,
                prompt: tests_prompt(crate::prompts::DirectivePrompt {
                    t,
                    cfg,
                    step,
                    n: attempt_no,
                    feedback,
                    journal: journal.as_deref(),
                    outcome: outcome.as_deref(),
                }),
                inputs: common,
                overlay_refs: Vec::new(),
                verify_ref: Some(format!("verify/{}", t.id)),
                scratch: Some(scratch_dir(&t.worktree)),
            }
        }
        Contract::Plan if step.action.name == "interview" => Spec {
            dir: PathBuf::from(&t.worktree),
            prompt: interview_prompt(
                t,
                cfg,
                step,
                &f.store.decisions_in_lineage(t.id).task()?,
                outcome.as_deref(),
            ),
            inputs: common,
            overlay_refs: Vec::new(),
            verify_ref: None,
            scratch: None,
        },
        Contract::Plan if step.action.name == "concierge" => Spec {
            dir: PathBuf::from(&t.worktree),
            prompt: concierge_prompt(f, t, cfg, step, outcome.as_deref()).task()?,
            inputs: common,
            overlay_refs: Vec::new(),
            verify_ref: None,
            scratch: None,
        },
        Contract::Plan => Spec {
            dir: PathBuf::from(&t.worktree),
            prompt: plan_prompt(crate::prompts::DirectivePrompt {
                t,
                cfg,
                step,
                n: attempt_no,
                feedback,
                journal: journal.as_deref(),
                outcome: outcome.as_deref(),
            }),
            inputs: common,
            overlay_refs: Vec::new(),
            verify_ref: None,
            scratch: None,
        },
        Contract::Review => Spec {
            dir: PathBuf::from(&t.worktree),
            prompt: review_prompt(t, cfg, step, outcome.as_deref()),
            // A review is told nothing of earlier attempts: it judges the
            // branch as it stands. Feedback owed to it is recorded, not shown.
            inputs: Inputs {
                journal: None,
                context: None,
                ..common
            },
            overlay_refs: Vec::new(),
            verify_ref: None,
            scratch: None,
        },
    };
    f.allow_egress(&spec.dir, cfg, t.trust);
    if let Some(scratch) = &spec.scratch {
        f.allow_egress(scratch, cfg, t.trust);
    }
    let mut inputs = spec.inputs;
    let mut spec_prompt = spec.prompt;
    if let Some(prev) = resume.and_then(|r| r.fresh_from.as_deref()) {
        let r = resume.expect("fresh_from implies a resume");
        spec_prompt.push_str(&crate::handoff::build(f, t, &spec.dir, &r.start_sha, prev).await);
    }
    inputs.prompt_chars = spec_prompt.chars().count();
    let provider = f
        .providers
        .get(&t.provider)
        .with_context(|| format!("task {}: unknown provider {:?}", t.id, t.provider))
        .env()?;
    let (mut a, log_path) = new_attempt(NewAttempt {
        f,
        t,
        step: &step.action.name,
        seq,
        dir: &spec.dir,
        attempt_no,
        inputs,
        resume,
        provider,
    })
    .await?;
    let outcome = launch(AttemptLaunch {
        f,
        t,
        step: &step.action.name,
        worktree: &spec.dir,
        prompt: &spec_prompt,
        log_path: &log_path,
        resume: resume
            .filter(|r| r.fresh_from.is_none())
            .map(|r| r.session.as_str()),
        writes: contract.writes(),
        start_sha: &a.start_sha,
        provider,
    })
    .await?;
    git::verification_checkout(
        &f.paths.home,
        repo,
        &spec.dir,
        spec.verify_ref.as_deref().unwrap_or(&t.branch),
        &t.base_branch,
    )
    .await
    .task()?;
    // A branch that merged the moved base is measured from there.
    let pending_main = match contract {
        Contract::Code => git::rev_parse(&spec.dir, &format!("refs/heads/forge/{}", t.base_branch))
            .await
            .ok(),
        _ => None,
    };
    // A directive scoped to its own paths keeps that scope; an unscoped
    // one inherits the task's project's scope for this repository, if any
    // (see docs/PROJECTS.md, "every task in the project inherits the
    // scope as its `--paths`").
    let project_scope;
    let (task_checks, paths, allow_protected): (&[String], &[String], bool) = match contract {
        Contract::Code => {
            let paths = if step.action.paths.is_empty() {
                project_scope = f.effective_paths(t);
                project_scope.as_slice()
            } else {
                step.action.paths.as_slice()
            };
            (&t.checks, paths, t.allow_protected)
        }
        _ => (&[], &[], false),
    };
    let verdict = verify::verify_directive(
        contract,
        &Subject {
            task_id: t.id,
            repo,
            worktree: &spec.dir,
            base_sha: &t.base_sha,
            start_sha: &a.start_sha,
            branch: &t.branch,
            cfg,
            task_checks,
            paths,
            allow_protected,
            overlay_refs: &spec.overlay_refs,
            pending_main: pending_main.as_deref(),
            sandbox: f.sandbox.as_ref(),
            report: &f.report,
            scratch: spec.scratch.as_deref(),
            plan_rows: !matches!(step.action.name.as_str(), "interview" | "concierge"),
        },
        &outcome,
    )
    .await
    .task()?;
    record(f, &mut a, &spec.dir, &verdict, &outcome, spec.verify_ref).await?;
    Ok((a, verdict, outcome))
}

pub fn tests_clone_dir(worktree: &str) -> PathBuf {
    PathBuf::from(format!("{worktree}-tests"))
}

fn scratch_dir(worktree: &str) -> PathBuf {
    PathBuf::from(format!("{worktree}-red"))
}

/// The model a step runs and its `inputs_json` records. A task names a
/// model in the Claude runner's vocabulary (`sonnet`, `opus`), so the
/// task's model applies only on that runner, except that a claude provider
/// other than the built-in "anthropic" which names a model of its own
/// (`[providers.anthropic-opus]`, `model = "opus"`) wins over the task's
/// default, so the economist can have an opus arm. A model the task
/// pinned explicitly (`pinned`: a `--model` flag, or a workflow step's own
/// `model`) still wins over the provider's. A step routed by role to a
/// provider on another runner takes that provider's configured model, or
/// the runner's own default when it has none (task 518's review, routed
/// to codex, asked OpenAI for `sonnet` and was refused). On the Claude
/// runner the supervisor is the one judge, never the routed work (see
/// `ctx::resolve_provider`), so it keeps its own configured model,
/// already on `requested` from `supervisor::supervise`.
pub(crate) fn attempt_model(
    step: &str,
    task_model: &str,
    requested: &str,
    provider: &agent::Provider,
    pinned: bool,
) -> String {
    match provider.runner {
        agent::Runner::ClaudeCli if step == "supervisor" => requested.to_string(),
        agent::Runner::ClaudeCli => match &provider.model {
            Some(m) if provider_model_applies(provider, pinned) => m.clone(),
            _ => task_model.to_string(),
        },
        _ => provider.model.clone().unwrap_or_default(),
    }
}

/// Whether a claude provider's own model beats the task's default.
fn provider_model_applies(provider: &agent::Provider, pinned: bool) -> bool {
    !pinned && provider.model.is_some() && provider.name != "anthropic"
}

/// Whether the task's model on this step was named explicitly: by `--model`
/// (`model_source` `"flag"`) or by the workflow step's own `model`, which
/// `engine::run_directive_step` marks with source `"step"` on its copy.
pub(crate) fn model_pinned(model_source: &str) -> bool {
    matches!(model_source, "flag" | "step")
}

/// Where the model `attempt_model` picked actually came from, for
/// `Task::routing` (see docs/ECONOMIST.md, "The routing record"): on the
/// Claude runner, a workflow step's own `model` wins with source
/// `"default"` (the action's own declared model, never a flag, a
/// project's, or the operator's); then a `--model` flag; then a claude
/// provider's own configured model, with source `"operator"` (only the
/// operator configures a provider's model); otherwise the task's model
/// applies, and `model_source` already names where that came from (see
/// `queue::enqueue`). Off the Claude runner, `step`'s override never
/// applies (`attempt_model` ignores it there too): the provider's own
/// configured model wins with source `"operator"`, else `"default"` when
/// the provider names none and the runner's own default applies.
pub(crate) fn attempt_model_source(
    step_model: Option<&str>,
    provider: &agent::Provider,
    model_source: &str,
) -> String {
    match provider.runner {
        agent::Runner::ClaudeCli if step_model.is_some() => "default".to_string(),
        agent::Runner::ClaudeCli if provider_model_applies(provider, model_source == "flag") => {
            "operator".to_string()
        }
        agent::Runner::ClaudeCli => model_source.to_string(),
        _ if provider.model.is_some() => "operator".to_string(),
        _ => "default".to_string(),
    }
}

/// Tool calls before the first edit in an attempt's stream: exploration.
fn first_edit_call(log_path: &Path) -> Option<i64> {
    let text = std::fs::read_to_string(log_path).ok()?;
    let mut seen = std::collections::HashSet::new();
    let mut calls = 0i64;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v["type"] != "assistant" {
            continue;
        }
        for b in v["message"]["content"].as_array().into_iter().flatten() {
            if b["type"] != "tool_use" {
                continue;
            }
            let id = b["id"].as_str().unwrap_or("").to_string();
            if !seen.insert(id) {
                continue;
            }
            if matches!(
                b["name"].as_str(),
                Some("Edit" | "Write" | "MultiEdit" | "NotebookEdit")
            ) {
                return Some(calls);
            }
            calls += 1;
        }
    }
    None
}

pub async fn new_attempt(args: NewAttempt<'_>) -> Result<(Attempt, PathBuf), Fault> {
    let NewAttempt {
        f,
        t,
        step,
        seq,
        dir,
        attempt_no,
        mut inputs,
        resume,
        provider,
    } = args;
    let log_path = f.paths.logs.join(format!("{}-{attempt_no}.jsonl", t.id));
    // A resumed attempt is measured from where the capped one began: the
    // agent's report covers the whole session.
    let start_sha = match resume {
        Some(r) => r.start_sha.clone(),
        None => git::head(dir).await.task()?,
    };
    // A supervisor may start in a fresh process, without run_task having
    // installed this worktree's executor yet. Always use its trusted base.
    let cfg = crate::config::load_at(Path::new(&t.repo), dir, &t.base_sha)
        .await
        .task()?;
    f.allow_egress(dir, &cfg, t.trust);
    let execution = f.execution_inputs(dir);
    inputs.executor = execution.executor;
    inputs.guarantees = execution.guarantees;
    inputs.workflow = t.workflow.clone();
    inputs.workflow_hash = t.workflow_hash.clone();
    inputs.step = step.to_string();
    inputs.model = attempt_model(
        step,
        &t.model,
        &inputs.model,
        provider,
        model_pinned(&t.model_source),
    );
    inputs.max_turns = t.max_turns;
    inputs.timeout_secs = t.timeout_secs;
    inputs.base_sha = t.base_sha.clone();
    inputs.start_sha = start_sha.clone();
    let mut a = Attempt {
        task_id: t.id,
        attempt_no,
        step: step.to_string(),
        step_seq: seq,
        start_sha,
        inputs_json: serde_json::to_string(&inputs).env()?,
        state: AttemptState::Running,
        started_at: unix_now(),
        log_path: log_path.display().to_string(),
        runner: provider.runner.as_str().to_string(),
        provider: provider.name.clone(),
        ..Default::default()
    };
    a.id = f.store.insert_attempt(&a).env()?;
    Ok((a, log_path))
}

async fn launch(args: AttemptLaunch<'_>) -> Result<agent::Outcome, Fault> {
    let AttemptLaunch {
        f,
        t,
        step,
        worktree,
        prompt,
        log_path,
        resume,
        writes,
        start_sha,
        provider,
    } = args;
    let model = attempt_model(
        step,
        &t.model,
        &t.model,
        provider,
        model_pinned(&t.model_source),
    );
    let outcome = crate::directive::launch(
        f,
        crate::directive::Spec {
            id: t.id,
            step,
            dir: worktree,
            prompt,
            system: "",
            model: &model,
            max_turns: t.max_turns as u32,
            timeout: Duration::from_secs(t.timeout_secs as u64),
            log_path,
            provider,
            schema: crate::envelope::SCHEMA,
            sandboxed: true,
            writes,
            start_sha,
            resume,
            no_tools: false,
        },
    )
    .await
    .env()?;
    f.report.emit(
        t.id,
        Event::AgentDone {
            exit: outcome.exit_code,
            turns: outcome.num_turns,
            tools: outcome.tool_calls,
            ms: outcome.wall_ms,
            cost: outcome.cost_usd,
            timed_out: outcome.timed_out,
        },
    );
    Ok(outcome)
}

pub async fn record(
    f: &Forge,
    a: &mut Attempt,
    dir: &Path,
    verdict: &Verdict,
    outcome: &agent::Outcome,
    verify_ref: Option<String>,
) -> Result<(), Fault> {
    let end_sha = git::head(dir).await.task()?;
    a.session_id = outcome.session_id.clone().unwrap_or_default();
    let mut outputs = Outputs {
        end_sha: end_sha.clone(),
        changed_files: git::changed_paths(dir, &a.start_sha).await.task()?,
        dirty_files: git::dirty_paths(dir).await.task()?,
        verify_ref: verify_ref.map(|r| format!("{r}@{end_sha}")),
        interface: if a.step == "tests" {
            verdict.envelope.as_ref().map(|e| e.summary.clone())
        } else {
            None
        },
        summary: verdict
            .envelope
            .as_ref()
            .map(|e| e.summary.clone())
            .unwrap_or_default(),
        claims: verdict.envelope.as_ref().map_or(0, |e| e.claims.len()),
        first_edit_call: first_edit_call(Path::new(&a.log_path)),
        tools: crate::tools::summarize(Path::new(&a.log_path), dir.to_str().unwrap_or("")),
        checks_run: verdict.envelope.as_ref().map_or(0, |e| e.checks_run.len()),
    };
    if let Some(tools) = &mut outputs.tools {
        let mut edited = outputs.changed_files.clone();
        edited.extend(outputs.dirty_files.iter().cloned());
        tools.tests = crate::tools::testruns::measure(Path::new(&a.log_path));
        tools.exploration = crate::tools::exploration::measure(
            Path::new(&a.log_path),
            dir.to_str().unwrap_or(""),
            &edited,
        );
    }
    a.end_sha = end_sha;
    a.first_edit = outputs.first_edit_call;
    a.outputs_json = serde_json::to_string(&outputs).env()?;
    a.state = verdict.state;
    a.reason = verdict.reason.clone();
    a.finished_at = Some(unix_now());
    a.agent_exit = outcome.exit_code;
    a.timed_out = outcome.timed_out;
    a.num_turns = outcome.num_turns;
    a.tool_calls = outcome.tool_calls;
    a.cost_usd = outcome.cost_usd;
    a.cli_cost_usd = outcome.cli_cost_usd;
    a.input_tokens = outcome.input_tokens;
    a.output_tokens = outcome.output_tokens;
    a.cache_read_input_tokens = outcome.cache_read_input_tokens;
    a.cache_creation_input_tokens = outcome.cache_creation_input_tokens;
    a.agent_ms = outcome.wall_ms as i64;
    a.commits = verdict.commits;
    a.files_changed = verdict.files_changed;
    a.dirty = verdict.dirty;
    a.verdict_json = serde_json::to_string(&verdict.checks).env()?;
    a.result_text = outcome.result_text.clone();
    a.envelope_json = verdict
        .envelope
        .as_ref()
        .and_then(|e| serde_json::to_string(e).ok())
        .unwrap_or_else(|| outcome.structured.clone().unwrap_or_default());
    a.rl_five_hour = outcome.rate_limits.five_hour.map(|(u, _)| u);
    a.rl_five_hour_resets = outcome.rate_limits.five_hour.map(|(_, r)| r);
    a.rl_seven_day = outcome.rate_limits.seven_day.map(|(u, _)| u);
    a.rl_seven_day_resets = outcome.rate_limits.seven_day.map(|(_, r)| r);
    a.early_signals = serde_json::to_string(&outcome.early_signals).env()?;
    a.early_near = serde_json::to_string(&outcome.early_near).env()?;
    f.store
        .finish_attempt(&FinishAttempt {
            id: a.id,
            state: a.state,
            reason: a.reason.clone(),
            finished_at: a.finished_at,
            agent_exit: a.agent_exit,
            timed_out: a.timed_out,
            num_turns: a.num_turns,
            tool_calls: a.tool_calls,
            cost_usd: a.cost_usd,
            cli_cost_usd: a.cli_cost_usd,
            agent_ms: a.agent_ms,
            commits: a.commits,
            files_changed: a.files_changed,
            dirty: a.dirty,
            verdict_json: a.verdict_json.clone(),
            result_text: a.result_text.clone(),
            envelope_json: a.envelope_json.clone(),
            rl_five_hour: a.rl_five_hour,
            rl_seven_day: a.rl_seven_day,
            rl_five_hour_resets: a.rl_five_hour_resets,
            rl_seven_day_resets: a.rl_seven_day_resets,
            end_sha: a.end_sha.clone(),
            outputs_json: a.outputs_json.clone(),
            session_id: a.session_id.clone(),
            first_edit: a.first_edit,
            input_tokens: a.input_tokens,
            output_tokens: a.output_tokens,
            cache_read_input_tokens: a.cache_read_input_tokens,
            cache_creation_input_tokens: a.cache_creation_input_tokens,
            early_signals: a.early_signals.clone(),
            early_near: a.early_near.clone(),
        })
        .env()?;
    f.report.emit(
        a.task_id,
        Event::AttemptDone {
            state: a.state.as_str(),
            reason: &a.reason,
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(name: &str, runner: agent::Runner, model: Option<&str>) -> agent::Provider {
        agent::Provider {
            name: name.into(),
            runner,
            model: model.map(str::to_string),
            base_url: None,
            api_key_env: None,
            env: vec![],
            extra_args: vec![],
            notes: None,
            price_input_per_million: 0.0,
            price_output_per_million: 0.0,
            price_cache_read_per_million: None,
            price_per_request: 0.0,
            five_hour_max: 0.0,
            seven_day_max: 0.0,
            nudges: 0,
        }
    }

    fn claude(model: Option<&str>) -> agent::Provider {
        provider("anthropic-opus", agent::Runner::ClaudeCli, model)
    }

    fn codex(model: Option<&str>) -> agent::Provider {
        provider("openai", agent::Runner::CodexCli, model)
    }

    #[test]
    fn attempt_model_keeps_the_supervisors_own_model() {
        assert_eq!(
            attempt_model(
                "supervisor",
                "task-model",
                "opus",
                &claude(Some("sonnet")),
                false
            ),
            "opus"
        );
        assert_eq!(
            attempt_model("supervisor", "sonnet", "opus", &codex(None), false),
            ""
        );
    }

    #[test]
    fn attempt_model_a_claude_providers_own_model_beats_the_tasks_default() {
        let p = claude(Some("opus"));
        assert_eq!(attempt_model("code", "sonnet", "sonnet", &p, false), "opus");
        assert_eq!(attempt_model_source(None, &p, "experiment"), "operator");
    }

    #[test]
    fn attempt_model_a_pinned_model_beats_the_claude_providers() {
        let p = claude(Some("opus"));
        assert_eq!(
            attempt_model("code", "sonnet", "sonnet", &p, true),
            "sonnet"
        );
        assert_eq!(attempt_model_source(None, &p, "flag"), "flag");
        assert_eq!(attempt_model_source(Some("sonnet"), &p, "step"), "default");
        assert!(model_pinned("flag") && model_pinned("step") && !model_pinned("project"));
    }

    #[test]
    fn attempt_model_a_claude_provider_without_a_model_takes_the_tasks() {
        let p = claude(None);
        assert_eq!(
            attempt_model("code", "sonnet", "sonnet", &p, false),
            "sonnet"
        );
        assert_eq!(attempt_model_source(None, &p, "project"), "project");
    }

    #[test]
    fn attempt_model_the_builtin_anthropic_keeps_the_tasks_model() {
        let p = provider("anthropic", agent::Runner::ClaudeCli, Some("sonnet"));
        assert_eq!(attempt_model("code", "opus", "opus", &p, false), "opus");
        assert_eq!(attempt_model_source(None, &p, "project"), "project");
    }

    #[test]
    fn attempt_model_another_runner_takes_the_providers_model_never_the_tasks() {
        assert_eq!(
            attempt_model("review", "sonnet", "sonnet", &codex(Some("gpt-5")), false),
            "gpt-5"
        );
        assert_eq!(
            attempt_model("review", "sonnet", "sonnet", &codex(None), true),
            ""
        );
    }
}
