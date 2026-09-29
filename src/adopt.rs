//! `forge adopt`: land a branch a human made outside Forge. No agent runs.
//! The branch is taken exactly as it is into a clean clone of the base,
//! judged by the repository's trusted checks the way an attempt's verify
//! judges an agent's work (setup first, then the rest, the protected-path
//! and namespace rules included), and, when it passes, landed through the
//! same integrator `forge land` uses: the base merged in, everything
//! re-verified, pushed and fast-forwarded under the repository's landing
//! lock. A failing check or a conflicting base blocks the task with the
//! output and a question for the human; nothing half-merged ever lands,
//! and nothing ever edits the adopted commits: a retry verifies again.
//! See docs/OPS.md, "Landing hand-made work".

use crate::checks::{self, CheckResult};
use crate::ctx::Forge;
use crate::engine::{OpRow, Timer, op};
use crate::envelope::{Envelope, Kind, NeedsInput};
use crate::landing::{self, LandOutcome};
use crate::report::Event;
use crate::store::{
    Adoption, Attempt, AttemptState, FinishAttempt, Origin, Task, TaskState, Trust,
};
use crate::verify::{self, Subject, Verdict};
use crate::{config, git, unix_now};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The workflow name every adopted task records: not a catalog workflow,
/// and never one an agent runs (`queue::enqueue` refuses it).
pub const WORKFLOW: &str = "adopt";

/// Why `name` is not a workflow a task can be queued under. `adopt` is
/// never in the catalog: an adopted task's retry or answer comes here
/// only if something tried to hand it to an agent, and is refused.
pub fn unknown_workflow(name: &str) -> String {
    if name == WORKFLOW {
        format!(
            "the {WORKFLOW} workflow is not an agent's: a hand-made branch is landed with forge adopt, and an adopted task is verified again with forge retry"
        )
    } else {
        format!("unknown workflow {name:?}; see `forge workflows`")
    }
}

/// What `forge adopt` was asked to do.
pub struct AdoptRequest {
    pub repo: PathBuf,
    /// A branch (local, or on the push remote) or a commit.
    pub rev: String,
    pub title: Option<String>,
    /// Verify only, and leave the verified branch for a human.
    pub no_land: bool,
    pub project: Option<String>,
    /// Let the branch change the repository's `[verify] protected` paths.
    pub allow_protected: bool,
    /// Who adopted it.
    pub by: String,
    /// The adopted task this one verifies again (`forge retry`).
    pub retry_of: Option<i64>,
}

/// How an adoption ended.
pub enum Adopted {
    /// On the base branch; `line` is the integrator's own.
    Landed { id: i64, line: String },
    /// Verified and left for a human (`--no-land`).
    Verified { id: i64 },
    /// A check failed, or landing with the base merged in did not work:
    /// the task is blocked with the reason and a question.
    Blocked { id: i64, reason: String },
    /// Refused before any check ran: the task failed with the reason.
    Refused { id: i64, reason: String },
}

/// The protected paths `changed` touches, unless the operator allowed
/// them: the rule `forge add --allow-protected` lifts for an agent's task,
/// applied to a human's branch the same way.
pub fn protected_hits(protected: &[String], changed: &[String], allow: bool) -> Vec<String> {
    if allow {
        return Vec::new();
    }
    changed
        .iter()
        .filter(|p| config::is_protected(protected, p))
        .cloned()
        .collect()
}

/// Where the adopted commit comes from: the commit, the branch name a
/// human gave (empty for a bare commit), and the source and full ref the
/// clone fetches it by.
struct Source {
    commit: String,
    branch: String,
    from: String,
    fetch_ref: String,
}

/// `rev` as a commit in the registered checkout (a branch, a remote
/// tracking branch, a tag or a sha), else as a branch on the push remote.
async fn resolve(repo: &Path, url: Option<&str>, rev: &str) -> Result<Source> {
    if let Ok(commit) = git::rev_parse(repo, &format!("{rev}^{{commit}}")).await {
        let bare_sha = rev.len() >= 4
            && rev.bytes().all(|b| b.is_ascii_hexdigit())
            && commit.starts_with(&rev.to_ascii_lowercase());
        return Ok(Source {
            branch: if bare_sha {
                String::new()
            } else {
                rev.to_string()
            },
            commit,
            from: repo.display().to_string(),
            // Set by `prepare` once the task has an id.
            fetch_ref: String::new(),
        });
    }
    if let Some(url) = url
        && let Some(commit) = git::remote_branch_sha(url, rev).await
    {
        return Ok(Source {
            commit,
            branch: rev.to_string(),
            from: url.to_string(),
            fetch_ref: format!("refs/heads/{rev}"),
        });
    }
    match url {
        Some(url) => bail!(
            "{rev} names no commit in {} and no branch on {url}",
            repo.display()
        ),
        None => bail!("{rev} names no commit in {}", repo.display()),
    }
}

/// The project the adopted task belongs to, as `forge add` decides it.
fn project_for(f: &Forge, repo: &str, named: Option<&str>) -> Result<String> {
    if let Some(p) = named {
        f.store
            .project(p)?
            .with_context(|| format!("no project {p}"))?;
        return Ok(p.to_string());
    }
    match f.store.ensure_default_project(repo)? {
        Some(name) => Ok(name),
        None => bail!(
            "{repo} is listed by several projects ({}); pass --project to say which",
            f.store.projects_listing_repo(repo)?.join(", ")
        ),
    }
}

/// Adopt `req.rev`: record it, verify it, and land it unless told not to.
pub async fn adopt(f: &Forge, req: &AdoptRequest) -> Result<Adopted> {
    let repo = req.repo.canonicalize().context("repo path")?;
    if !repo.join(".git").exists() {
        bail!("{} is not a git repository", repo.display());
    }
    let repo_str = repo.display().to_string();
    let project = project_for(f, &repo_str, req.project.as_deref())?;
    let cfg = config::load_working(&repo).await?;
    if cfg.checks.is_empty() {
        bail!(
            "{} declares no [checks]; nothing would verify the adopted branch",
            cfg.config_path
        );
    }
    let remote = match &cfg.push_remote {
        Some(name) => git::remote_url(&repo, name)
            .await
            .map(|url| (name.clone(), url)),
        None => None,
    };
    if !req.no_land && remote.is_none() {
        bail!(
            "{} has no push remote; nothing to land on (adopt with --no-land to verify only)",
            repo.display()
        );
    }
    let mut src = resolve(&repo, remote.as_ref().map(|(_, u)| u.as_str()), &req.rev).await?;
    let subject = git::subject(&repo, &src.commit).await.unwrap_or_default();
    let title = req.title.clone().filter(|t| !t.trim().is_empty());
    let name = if src.branch.is_empty() {
        src.commit[..8].to_string()
    } else {
        src.branch.clone()
    };
    // A retry of an adopted task (`forge retry`, via `adopt::retry`)
    // inherits its predecessor's priority, same as an agent-run retry
    // (`queue::retry_request`); a fresh `forge adopt` gets the default.
    let priority = match req.retry_of {
        Some(old) => f
            .store
            .task(old)?
            .map_or(crate::store::PRIORITY_DEFAULT, |t| t.priority),
        None => crate::store::PRIORITY_DEFAULT,
    };
    let mut t = Task {
        repo: repo_str,
        task: format!(
            "Adopt {name} @ {} as it is: {}",
            &src.commit[..8],
            title.as_deref().unwrap_or(&subject)
        ),
        title: title.or_else(|| (!subject.is_empty()).then(|| subject.clone())),
        base_branch: cfg.base_branch.clone(),
        max_attempts: 1,
        state: TaskState::Queued,
        created_at: unix_now(),
        allow_protected: req.allow_protected,
        workflow: WORKFLOW.to_string(),
        workflow_source: "flag".to_string(),
        land: !req.no_land,
        retry_of: req.retry_of,
        journal: false,
        journal_arm: "explicit".to_string(),
        project: Some(project),
        trust: Trust::Operator,
        origin: Origin::Adopted,
        adoption: Some(Adoption {
            branch: src.branch.clone(),
            commit: src.commit.clone(),
            by: req.by.clone(),
        }),
        priority,
        ..Default::default()
    };
    t.id = f.store.insert_task(&t)?;
    // Queued only for the instant it takes to claim it: no worker ever
    // claims an adopted task (`Store::queued_unblocked`), and one left
    // running by a `forge adopt` that died blocks rather than requeues.
    if !f.store.claim(t.id, std::process::id() as i64)? {
        bail!("task {} could not be claimed", t.id);
    }
    t = f.store.task(t.id)?.context("the task went away")?;
    t.branch = format!("forge/{}-adopt-{}", t.id, crate::engine::slug(&name));
    t.worktree = f
        .paths
        .worktrees
        .join(t.id.to_string())
        .display()
        .to_string();
    f.store.update_task(&t)?;
    f.report.emit(
        t.id,
        Event::TaskQueued {
            workflow: WORKFLOW,
            retry_of: req.retry_of,
        },
    );
    if src.fetch_ref.is_empty() {
        // Kept, so the adopted commit outlives the branch it came on.
        src.fetch_ref = format!("refs/forge/adopted/{}", t.id);
        git::update_ref(&repo, &src.fetch_ref, &src.commit).await?;
    }
    let result = run(f, &mut t, &cfg, remote.as_ref(), &src, req).await;
    if let Err(e) = &result {
        let reason = format!(
            "adoption stopped: {e:#}; forge retry {} verifies it again",
            t.id
        );
        block(f, &mut t, &reason, None)?;
    }
    result
}

/// Everything after the record exists: the clone, the protected-path
/// rule, the checks, and the landing.
async fn run(
    f: &Forge,
    t: &mut Task,
    cfg: &config::Config,
    remote: Option<&(String, String)>,
    src: &Source,
    req: &AdoptRequest,
) -> Result<Adopted> {
    let repo = PathBuf::from(&t.repo);
    let wt = PathBuf::from(&t.worktree);
    if wt.exists() {
        std::fs::remove_dir_all(&wt)?;
    }
    let timer = Timer::now();
    let mut tip = git::clone_task(&repo, &cfg.base_branch, &wt, &t.branch, None, None).await?;
    // The base as the remote has it, fetched straight into the clone: the
    // registered checkout, the human's own, is only ever read.
    if let Some((_, url)) = remote
        && git::remote_branch_exists(url, &cfg.base_branch).await?
    {
        let base_ref = format!("refs/heads/{}", cfg.base_branch);
        git::fetch_full_ref(&wt, url, &base_ref).await?;
        tip = git::rev_parse(&wt, "FETCH_HEAD").await?;
    }
    git::fetch_full_ref(&wt, &src.from, &src.fetch_ref).await?;
    git::reset_hard(&wt, &src.commit).await?;
    let Some(base_sha) = git::merge_base(&wt, &src.commit, &tip).await else {
        let reason = format!(
            "refused: {} shares no history with {}",
            &src.commit[..8],
            t.base_branch
        );
        return refuse(f, t, &reason);
    };
    if git::is_ancestor(&wt, &src.commit, &tip).await {
        let reason = format!(
            "refused: {} is already on {}; nothing to land",
            &src.commit[..8],
            t.base_branch
        );
        return refuse(f, t, &reason);
    }
    t.base_sha = base_sha.clone();
    f.store.update_task(t)?;
    let detail = format!(
        "adopted {} @ {} onto {} @ {}",
        src.from,
        &src.commit[..8],
        t.base_branch,
        &base_sha[..8]
    );
    op(f, t.id, &timer, row(1, "clone", true, &detail)).map_err(fault)?;
    let cfg_base = config::load_at(&repo, &wt, &base_sha).await?;
    let changed = git::changed_paths(&wt, &base_sha).await?;
    // The repository's own config is always protected, as it is for an
    // agent's attempt: the checks come from the trusted base.
    let mut guarded = cfg_base.protected.clone();
    guarded.push(cfg_base.config_path.clone());
    let hits = protected_hits(&guarded, &changed, req.allow_protected);
    if let Some(reason) = record_protected(f, t, &guarded, &changed, &hits)? {
        return refuse(f, t, &reason);
    }
    let v = verify_as_is(f, t, &cfg_base, &base_sha, &tip, &src.commit).await?;
    if v.state != AttemptState::Succeeded {
        let reason = failing_reason(&v, src, t.id);
        let question = failure_question(&v, src, t);
        block(f, t, &reason, Some(question))?;
        return Ok(Adopted::Blocked { id: t.id, reason });
    }
    t.state = TaskState::Succeeded;
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    if req.no_land {
        if let Some((_, url)) = remote {
            git::push(&f.paths.home, &repo, &wt, url, &t.branch).await?;
            t.pushed = true;
        }
        t.reason = format!(
            "verified; left for a human (--no-land): forge land {} lands it",
            t.id
        );
        f.store.update_task(t)?;
        return Ok(Adopted::Verified { id: t.id });
    }
    t.reason = "verified; landing".to_string();
    f.store.update_task(t)?;
    land(f, t, src).await
}

/// The integrator, as `forge land` runs it; what it could not do blocks
/// the task instead of going back to a coder.
async fn land(f: &Forge, t: &mut Task, src: &Source) -> Result<Adopted> {
    let (reason, tried) = match landing::land_task_outcome(f, t.id, true).await? {
        LandOutcome::Landed(line) => return Ok(Adopted::Landed { id: t.id, line }),
        LandOutcome::Rewind { first, feedback } => (first, feedback),
        LandOutcome::Failed(reason) => (reason.clone(), reason),
    };
    *t = f.store.task(t.id)?.context("the task went away")?;
    let source = source_name(src);
    let question = NeedsInput {
        question: format!(
            "The adopted branch {source} passed its checks, but landing it with the current {base} merged in did not: {reason}. Nothing landed and the branch is untouched. Merge {base} into {source} by hand (resolving any conflict), push it, and run `forge retry {id}` to verify it again; or `forge withdraw {id}`.",
            base = t.base_branch,
            id = t.id,
        ),
        tried: format!(
            "Verified {source} @ {} as it is (no agent ran), then ran the integrator: {tried}",
            &src.commit[..8]
        ),
        ..needs_input_base()
    };
    let reason = format!(
        "landing failed: {reason}; fix {source} and forge retry {}",
        t.id
    );
    block(f, t, &reason, Some(question))?;
    Ok(Adopted::Blocked { id: t.id, reason })
}

fn source_name(src: &Source) -> String {
    if src.branch.is_empty() {
        src.commit[..8].to_string()
    } else {
        src.branch.clone()
    }
}

fn needs_input_base() -> NeedsInput {
    NeedsInput {
        question: String::new(),
        tried: String::new(),
        path: String::new(),
        kind: Kind::Question,
        options: vec!["forge retry".into(), "forge withdraw".into()],
        context: String::new(),
        checkpoint: None,
        to: None,
    }
}

/// Record the protected-path ruling as a decision: a refusal when the
/// branch touches them without `--allow-protected` (the reason is
/// returned), the operator's allowance when it touches them with it.
fn record_protected(
    f: &Forge,
    t: &Task,
    protected: &[String],
    changed: &[String],
    hits: &[String],
) -> Result<Option<String>> {
    let allowed: Vec<&String> = changed
        .iter()
        .filter(|p| config::is_protected(protected, p))
        .collect();
    let (answer, refusal) = if !hits.is_empty() {
        let reason = format!(
            "refused: the adopted branch changes protected paths: {}. Only an adoption with --allow-protected may change them",
            hits.join(", ")
        );
        (reason.clone(), Some(reason))
    } else if !allowed.is_empty() && t.allow_protected {
        let names: Vec<&str> = allowed.iter().map(|s| s.as_str()).collect();
        (
            format!(
                "allowed (--allow-protected): the adopted branch changes {}",
                names.join(", ")
            ),
            None,
        )
    } else {
        return Ok(None);
    };
    let decision = f.store.insert_decision_by(crate::store::InsertDecisionBy {
        task_id: t.id,
        repo: &t.repo,
        question: &format!("adopt task {}: may it change protected paths?", t.id),
        answer: &answer,
        answered_by: "operator",
        citations: "",
        answered_for: None,
    })?;
    f.store.set_decision_kind(decision, "protected-paths")?;
    Ok(refusal)
}

/// The repository's checks on the branch as it is, recorded as the task's
/// one attempt (step `adopt`: no agent, no turns, no cost).
async fn verify_as_is(
    f: &Forge,
    t: &Task,
    cfg: &config::Config,
    base_sha: &str,
    tip: &str,
    commit: &str,
) -> Result<Verdict> {
    let repo = PathBuf::from(&t.repo);
    let wt = PathBuf::from(&t.worktree);
    f.allow_egress(&wt, cfg, t.trust, None);
    // The standing hidden suite describes the base as it is now: only a
    // branch that already contains it is judged by it here. Landing, with
    // the base merged in, is judged by it either way.
    let pinned = if base_sha == tip { None } else { Some("") };
    let overlay = landing::overlay_refs(&repo, t.id, pinned).await;
    let timer = Timer::now();
    let inputs = crate::audit::Inputs {
        step: WORKFLOW.to_string(),
        base_sha: base_sha.to_string(),
        start_sha: base_sha.to_string(),
        ..f.execution_inputs(&wt)
    };
    let mut a = Attempt {
        task_id: t.id,
        attempt_no: 1,
        step: WORKFLOW.to_string(),
        step_seq: 2,
        start_sha: base_sha.to_string(),
        inputs_json: serde_json::to_string(&inputs)?,
        state: AttemptState::Running,
        started_at: unix_now(),
        ..Default::default()
    };
    a.id = f.store.insert_attempt(&a)?;
    let v = verify::verify_operation(Subject {
        task_id: t.id,
        repo: &repo,
        worktree: &wt,
        base_sha,
        start_sha: base_sha,
        branch: &t.branch,
        cfg,
        task_checks: &[],
        paths: &[],
        allow_protected: t.allow_protected,
        overlay_refs: &overlay,
        pending_main: None,
        sandbox: f.sandbox.as_ref(),
        report: &f.report,
        logs_dir: &f.paths.logs,
        scratch: None,
        plan_rows: false,
    })
    .await?;
    finish(f, a.id, &v, commit)?;
    let detail = if v.state == AttemptState::Succeeded {
        format!("{} @ {} verified as it is", t.branch, &commit[..8])
    } else {
        v.reason.clone()
    };
    op(
        f,
        t.id,
        &timer,
        OpRow {
            attempt_id: Some(a.id),
            ..row(2, "verify", v.state == AttemptState::Succeeded, &detail)
        },
    )
    .map_err(fault)?;
    Ok(v)
}

fn finish(f: &Forge, id: i64, v: &Verdict, commit: &str) -> Result<()> {
    f.store.finish_attempt(&FinishAttempt {
        id,
        state: v.state,
        reason: v.reason.clone(),
        finished_at: Some(unix_now()),
        agent_exit: None,
        timed_out: false,
        num_turns: 0,
        tool_calls: 0,
        cost_usd: None,
        agent_ms: 0,
        commits: v.commits,
        files_changed: v.files_changed,
        dirty: v.dirty,
        verdict_json: serde_json::to_string(&v.checks)?,
        result_text: String::new(),
        envelope_json: String::new(),
        rl_five_hour: None,
        rl_seven_day: None,
        rl_five_hour_resets: None,
        rl_seven_day_resets: None,
        end_sha: commit.to_string(),
        outputs_json: serde_json::to_string(&crate::audit::Outputs::default())?,
        session_id: String::new(),
        first_edit: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        early_signals: "[]".to_string(),
        early_near: "[]".to_string(),
        cli_cost_usd: None,
    })
}

fn row<'a>(seq: i64, name: &'a str, ok: bool, detail: &'a str) -> OpRow<'a> {
    OpRow {
        seq,
        name,
        kernel: true,
        ok,
        exit: None,
        detail,
        attempt_id: None,
        output: "",
    }
}

fn fault(e: crate::engine::Fault) -> anyhow::Error {
    match e {
        crate::engine::Fault::Task(e) | crate::engine::Fault::Env(e) => e,
    }
}

fn failed_checks(v: &Verdict) -> Vec<&CheckResult> {
    v.checks.iter().filter(|c| !c.ok).collect()
}

/// The task's reason when the branch fails as it is: which checks.
fn failing_reason(v: &Verdict, src: &Source, id: i64) -> String {
    let names: Vec<&str> = failed_checks(v).iter().map(|c| c.name.as_str()).collect();
    format!(
        "check {} failed on the adopted branch {}; fix the branch and forge retry {id}",
        names.join(", "),
        source_name(src)
    )
}

fn failure_question(v: &Verdict, src: &Source, t: &Task) -> NeedsInput {
    let source = source_name(src);
    let tails: Vec<String> = failed_checks(v)
        .iter()
        .map(|c| {
            format!(
                "- {} {}:\n{}",
                c.level,
                c.name,
                checks::last_lines(&c.tail, 20)
            )
        })
        .collect();
    NeedsInput {
        question: format!(
            "The adopted branch {source} @ {} fails {} as it is, so it did not land. No agent will change it: fix the branch by hand, push it, and run `forge retry {id}` to verify it again; or `forge withdraw {id}`.",
            &src.commit[..8],
            failed_checks(v)
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            id = t.id,
        ),
        tried: format!(
            "Ran the repository's checks on {source} @ {} in a clean worktree, unchanged:\n{}",
            &src.commit[..8],
            tails.join("\n")
        ),
        ..needs_input_base()
    }
}

/// Block `t` with `reason`, and the question on its last attempt when
/// there is one, so `forge requests` and `forge show` read it.
fn block(f: &Forge, t: &mut Task, reason: &str, question: Option<NeedsInput>) -> Result<()> {
    if let Some(q) = question
        && let Some(a) = f.store.attempts(t.id)?.last()
    {
        let env = Envelope {
            schema_version: 1,
            summary: reason.to_string(),
            needs_input: Some(q),
            changes: Vec::new(),
            checks_run: Vec::new(),
            claims: Vec::new(),
            review_notes: Vec::new(),
        };
        f.store
            .set_attempt_envelope(a.id, &serde_json::to_string(&env)?)?;
    }
    t.state = TaskState::Blocked;
    t.reason = reason.to_string();
    t.worker_pid = None;
    f.store.update_task(t)?;
    f.report.emit(t.id, Event::Note { text: reason });
    crate::audience::emit_ended(f, t)
}

/// End the task before any check ran: the branch is not adoptable.
fn refuse(f: &Forge, t: &mut Task, reason: &str) -> Result<Adopted> {
    t.state = TaskState::Failed;
    t.reason = reason.to_string();
    t.finished_at = Some(unix_now());
    t.worker_pid = None;
    f.store.update_task(t)?;
    f.report.emit(t.id, Event::Note { text: reason });
    let _ = std::fs::remove_dir_all(&t.worktree);
    crate::sandbox::discard_provider_state(Path::new(&t.worktree));
    crate::audience::emit_ended(f, t)?;
    Ok(Adopted::Refused {
        id: t.id,
        reason: reason.to_string(),
    })
}

/// `forge retry` of an adopted task: adopt the same branch again (its
/// current commit, if it moved since) or the same commit, as a new task
/// that retries it. Never an agent run.
pub async fn retry(f: &Forge, old: &Task, by: &str) -> Result<Adopted> {
    let Some(a) = &old.adoption else {
        bail!("task {} was not adopted", old.id);
    };
    if matches!(old.state, TaskState::Queued | TaskState::Running) {
        bail!(
            "task {} is {}; only a finished adoption is retried",
            old.id,
            old.state.as_str()
        );
    }
    let req = AdoptRequest {
        repo: PathBuf::from(&old.repo),
        rev: a.source().to_string(),
        title: old.title.clone(),
        no_land: !old.land,
        project: old.project.clone(),
        allow_protected: old.allow_protected,
        by: by.to_string(),
        retry_of: Some(old.id),
    };
    adopt(f, &req).await
}

/// Who is adopting: the login name, else "operator".
pub fn adopter() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

/// What `forge adopt` prints, ending with what the human does next.
pub async fn render(f: &Forge, adopted: &Adopted) -> Result<String> {
    let id = match adopted {
        Adopted::Landed { id, .. }
        | Adopted::Verified { id }
        | Adopted::Blocked { id, .. }
        | Adopted::Refused { id, .. } => *id,
    };
    let t = f.store.task(id)?.with_context(|| format!("no task {id}"))?;
    let mut lines = vec![format!(
        "task     {id}: {}",
        t.adoption
            .as_ref()
            .map(|a| a.describe())
            .unwrap_or_default()
    )];
    let checks = f
        .store
        .attempts(id)?
        .into_iter()
        .find(|a| a.step == WORKFLOW)
        .map(|a| serde_json::from_str::<Vec<CheckResult>>(&a.verdict_json).unwrap_or_default())
        .unwrap_or_default();
    for c in checks.iter().filter(|c| c.level != "L0") {
        lines.push(format!(
            "check    {} {} {}",
            c.level,
            c.name,
            if c.ok { "ok" } else { "FAIL" }
        ));
    }
    let pull = format!(
        "git -C {} pull --ff-only {} {}",
        t.repo,
        config::load_working(Path::new(&t.repo))
            .await
            .ok()
            .and_then(|c| c.push_remote)
            .unwrap_or_else(|| "origin".to_string()),
        t.base_branch
    );
    match adopted {
        Adopted::Landed { line, .. } => {
            lines.push(line.clone());
            lines.push(format!(
                "Update your working copy (on {}): {pull}",
                t.base_branch
            ));
        }
        Adopted::Verified { .. } => {
            lines.push(format!("verified task {id}: {}", t.reason));
            lines.push(format!(
                "After `forge land {id}`, update your working copy (on {}): {pull}",
                t.base_branch
            ));
        }
        Adopted::Blocked { reason, .. } => {
            lines.push(format!("blocked task {id}: {reason}"));
        }
        Adopted::Refused { reason, .. } => {
            lines.push(format!("task {id} {reason}"));
        }
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_branch_touching_a_protected_path_is_refused_without_the_flag() {
        let protected = s(&["forge.toml", "tests/boundary.rs"]);
        let changed = s(&["src/lib.rs", "forge.toml"]);
        assert_eq!(
            protected_hits(&protected, &changed, false),
            s(&["forge.toml"])
        );
    }

    #[test]
    fn allow_protected_lifts_the_rule() {
        let protected = s(&["forge.toml"]);
        let changed = s(&["forge.toml"]);
        assert!(protected_hits(&protected, &changed, true).is_empty());
    }

    #[test]
    fn a_branch_away_from_protected_paths_passes_the_rule() {
        let protected = s(&["forge.toml", "tests/boundary.rs"]);
        let changed = s(&["src/physics.rs", "tests/park.rs"]);
        assert!(protected_hits(&protected, &changed, false).is_empty());
    }
}
