use super::*;
use crate::workflows;

pub(super) fn trace(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}")
    };
    let doc = crate::view::trace_doc(&f, &t)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    out!("task {}  {}  {}", t.id, t.state.as_str(), t.reason);
    out!("trust      {}", t.trust.as_str());
    out!("repo       {}", t.repo);
    out!(
        "branch     {} from {} @ {}",
        t.branch,
        t.base_branch,
        t.base_sha
    );
    out!("workflow   {} {}", t.workflow, t.workflow_hash);
    for l in t.workflow_text.lines() {
        out!("  | {l}");
    }
    out!("text       {}", t.task);
    out!(
        "shape      text_len={} path_tokens={} tdd={} declared_checks={}",
        t.shape_text_len,
        t.shape_path_tokens,
        t.shape_tdd,
        t.shape_declared_checks
    );
    for (role, r) in &doc.task.routing {
        out!(
            "routing    {role:<8} provider={}({}) model={}({}) workflow={}({})",
            r.provider.value,
            r.provider.source,
            r.model.value,
            r.model.source,
            r.workflow.value,
            r.workflow.source
        );
    }
    if let Ok(r) = serde_json::from_value::<workflows::Resolved>(doc.resolved.clone()) {
        out!(
            "resolved   {}",
            r.steps
                .iter()
                .map(|s| format!("{}@{}", s.action.name, &s.action.hash[..8]))
                .collect::<Vec<_>>()
                .join(" → ")
        );
        for p in &r.pins {
            out!("  pin      {:<9} {:<10} {}", p.kind, p.name, p.hash);
        }
    }
    for o in doc.ops.iter().filter(|o| o.attempt_id.is_none()) {
        out!(
            "op         {} seq {} {}{} {:.1}s {}",
            if o.ok { "✓" } else { "✗" },
            o.seq,
            o.name,
            if o.kernel { "" } else { " [user]" },
            o.ms as f64 / 1000.0,
            o.detail.lines().next().unwrap_or("")
        );
        for l in o.output.lines().take(12) {
            out!("           > {l}");
        }
    }
    for a in &doc.attempts {
        out!();
        out!(
            "=== attempt {} [{} seq {}] {}{}",
            a.attempt_no,
            a.step,
            a.step_seq,
            a.state,
            if a.reason.is_empty() {
                String::new()
            } else {
                format!(": {}", a.reason)
            }
        );
        let inputs: audit::Inputs = serde_json::from_value(a.inputs.clone()).unwrap_or_default();
        out!(
            "inputs     model={} runner={} provider={} max_turns={} timeout={}s base={} start={}",
            inputs.model,
            a.runner,
            a.provider,
            inputs.max_turns,
            inputs.timeout_secs,
            &inputs.base_sha[..inputs.base_sha.len().min(8)],
            &inputs.start_sha[..inputs.start_sha.len().min(8)]
        );
        out!(
            "           checks_shown={} task_checks={:?} protected={:?} namespace={:?} overlay={:?} prompt_chars={}",
            inputs.checks_shown,
            inputs.task_checks,
            inputs.protected,
            inputs.namespace,
            inputs.overlay_refs,
            inputs.prompt_chars
        );
        if let Some(i) = &inputs.interface {
            out!("interface  {}", i.lines().collect::<Vec<_>>().join(" / "));
        }
        if let Some(fb) = &inputs.feedback {
            out!("feedback   |");
            for l in fb.lines() {
                out!("           | {l}");
            }
        }
        out!(
            "agent      exit {} turns {} tools {} {:.1}s {}{}",
            a.agent_exit.map_or("-".into(), |v| v.to_string()),
            a.num_turns,
            a.tool_calls,
            a.agent_ms as f64 / 1000.0,
            a.cost_usd.map_or("-".into(), |c| format!("${c:.4}")),
            if a.timed_out { " TIMED OUT" } else { "" }
        );
        if let Ok(rows) =
            serde_json::from_value::<Vec<crate::checks::CheckResult>>(a.verdict.clone())
        {
            for c in rows {
                out!(
                    "verdict    {} {} {} ({:.1}s){}",
                    if c.ok { "✓" } else { "✗" },
                    c.level,
                    c.name,
                    c.ms as f64 / 1000.0,
                    if c.failing_tests.is_empty() {
                        String::new()
                    } else {
                        format!(" failing: {}", c.failing_tests.join(", "))
                    }
                );
                if !c.ok {
                    for l in crate::checks::last_lines(&c.tail, 40).lines() {
                        out!("           | {l}");
                    }
                }
            }
        }
        let outputs: audit::Outputs = serde_json::from_value(a.outputs.clone()).unwrap_or_default();
        out!(
            "outputs    end={} changed={:?} dirty={:?} claims={} checks_run={}",
            &outputs.end_sha[..outputs.end_sha.len().min(8)],
            outputs.changed_files,
            outputs.dirty_files,
            outputs.claims,
            outputs.checks_run
        );
        if let Some(r) = &outputs.verify_ref {
            out!("           verify_ref={r}");
        }
        if !outputs.refused.is_empty() {
            let hosts: Vec<String> = outputs.refused.iter().map(|r| r.to_string()).collect();
            out!("refused    {}", hosts.join(", "));
        }
        if !outputs.summary.is_empty() {
            out!(
                "summary    {}",
                outputs.summary.lines().collect::<Vec<_>>().join(" / ")
            );
        }
        out!("log        {}", a.log_path);
    }
    for dgn in &doc.diagnosis {
        out!();
        out!("what       {}", dgn.what);
        out!("action     {}", dgn.action);
    }
    Ok(())
}

/// `forge log`'s filters, gathered into one struct so the function that
/// applies them stays under clippy's argument-count lint.
pub(super) struct LogArgs {
    pub(super) limit: u32,
    pub(super) state: Option<String>,
    pub(super) repo: Option<PathBuf>,
    pub(super) before: Option<i64>,
    pub(super) grep: Option<String>,
    pub(super) workflow: Option<String>,
    pub(super) project: Option<String>,
    pub(super) initiative: Option<i64>,
    pub(super) touches: Vec<String>,
    pub(super) touches_text: bool,
    pub(super) failed_on: Vec<String>,
    pub(super) reason: Option<String>,
}

pub(super) fn log(args: LogArgs, json: bool) -> Result<()> {
    let LogArgs {
        limit,
        state,
        repo,
        before,
        grep,
        workflow,
        project,
        initiative,
        touches,
        touches_text,
        failed_on,
        reason,
    } = args;
    let state = state
        .map(|s| {
            TaskState::try_from(s.as_str()).map_err(|_| {
                anyhow::anyhow!(
                    "unknown state {s:?}; valid states are queued, running, succeeded, failed, blocked, unverified"
                )
            })
        })
        .transpose()?;
    let repo = repo
        .map(|p| p.canonicalize().context("repo path"))
        .transpose()?
        .map(|p| p.display().to_string());
    let f = Forge::open(false, false)?;
    let q = crate::store::TaskFilter {
        limit,
        state,
        repo,
        before,
        grep,
        workflow,
        project,
        initiative,
        touches,
        touches_text,
        failed_on,
        reason,
    };
    let rows = tasks_json(&f, &q)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    out!(
        "{:<5} {:<11} {:<8} {:<7} {:<3} {:<8} {:<19} {:<18} TASK",
        "ID",
        "STATE",
        "TRUST",
        "WF",
        "ATT",
        "COST",
        "CREATED",
        "REPO"
    );
    for s in &rows {
        let repo_name = Path::new(&s.repo)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.repo.clone());
        let task_short: String = s
            .task
            .chars()
            .take(50)
            .collect::<String>()
            .replace('\n', " ");
        out!(
            "{:<5} {:<11} {:<8} {:<7} {:<3} {:<8} {:<20} {:<18} {}{}{}{}",
            s.id,
            s.state,
            s.trust,
            s.workflow,
            s.attempts,
            format!("${:.4}", s.cost_usd),
            render::utc(s.created_at),
            repo_name,
            task_short,
            if s.touch.as_deref() == Some("text") {
                " (by text)"
            } else {
                ""
            },
            match s.matched.as_deref() {
                Some(m) if m != "text" => format!(" (by {m})"),
                _ => String::new(),
            },
            failed_suffix(&s.failures)
        );
    }
    Ok(())
}

/// The listing's mark for a `--failed-on`/`--reason` match: the distinct
/// failing row names, `reason` for a reason match; empty without one.
fn failed_suffix(failures: &[crate::store::FailedAttempt]) -> String {
    if failures.is_empty() {
        return String::new();
    }
    let mut names: Vec<&str> = Vec::new();
    for f in failures {
        let n = f.name.as_deref().unwrap_or("reason");
        if !names.contains(&n) {
            names.push(n);
        }
    }
    format!(" (failed: {})", names.join(", "))
}

pub(super) fn show(id: i64, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let Some(t) = f.store.task(id)? else {
        bail!("no task {id}")
    };
    let doc = crate::view::trace_doc(&f, &t)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&doc.task)?);
        return Ok(());
    }
    let task = &doc.task;
    let cost: f64 = doc.attempts.iter().filter_map(|a| a.cost_usd).sum();
    out!("task       {}", task.id);
    out!(
        "state      {}{}",
        task.state,
        if task.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", task.reason)
        }
    );
    out!("trust      {}", t.trust.as_str());
    out!("repo       {}", task.repo);
    out!("created    {}", render::utc(task.created_at));
    if let Some(pname) = &t.project {
        out!("project    {pname}");
    }
    let effective_supervisor = f.effective_supervisor(&t);
    let scope = f.effective_paths(&t);
    out!(
        "defaults   per-task cap ${:.2}, supervisor {} (per-lineage {}){}",
        f.effective_per_task_usd(&t),
        effective_supervisor.model,
        effective_supervisor.per_lineage,
        if scope.is_empty() {
            String::new()
        } else {
            format!(", scope {}", scope.join(", "))
        }
    );
    out!(
        "base       {} @ {}",
        task.base_branch,
        if task.base_sha.is_empty() {
            "-"
        } else {
            &task.base_sha[..8]
        }
    );
    out!(
        "branch     {}{}",
        if task.branch.is_empty() {
            "-"
        } else {
            &task.branch
        },
        if task.pushed { " (pushed)" } else { "" }
    );
    out!(
        "worktree   {}{}",
        if task.worktree.is_empty() {
            "-"
        } else {
            &task.worktree
        },
        if task.worktree_removed_at.is_some() {
            " (removed)"
        } else {
            ""
        }
    );
    out!(
        "model      {} (max {} turns, max {} attempts, {}s timeout)",
        task.model,
        task.max_turns,
        task.max_attempts,
        task.timeout_secs
    );
    out!(
        "provider   {}",
        if task.provider.is_empty() {
            "(per-role; see forge providers)"
        } else {
            &task.provider
        }
    );
    out!(
        "cost       ${cost:.4} over {} attempt(s){}",
        doc.attempts.len(),
        task.budget_usd
            .map_or(String::new(), |b| format!(" (task cap ${b:.2})"))
    );
    for c in &task.checks {
        out!("check      $ {c}");
    }
    if task.allow_protected {
        out!("protected  changes allowed");
    }
    if !task.land {
        out!("land       manual: the verified branch is left for a human");
    }
    out!(
        "journal    {} ({})",
        if task.journal_enabled { "on" } else { "off" },
        task.journal_arm
    );
    if !task.explore.is_empty() {
        out!(
            "explore    {}",
            task.explore
                .iter()
                .map(|(role, provider)| format!("{role}={provider}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !task.routing.is_empty() {
        out!(
            "routing    {}",
            task.routing
                .iter()
                .map(|(role, r)| format!(
                    "{role}: provider={}({}) model={}({}) workflow={}({})",
                    r.provider.value,
                    r.provider.source,
                    r.model.value,
                    r.model.source,
                    r.workflow.value,
                    r.workflow.source
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !task.after.is_empty() {
        out!(
            "after      {}",
            task.after
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(r) = task.retry_of {
        out!("retry of   {r}");
    }
    if task.lineage.len() > 1 {
        out!(
            "lineage    {}",
            task.lineage
                .iter()
                .map(|l| if l.id == task.id {
                    format!("[{} {}]", l.id, l.state)
                } else {
                    format!("{} {}", l.id, l.state)
                })
                .collect::<Vec<_>>()
                .join(" → ")
        );
    }
    if let Some(a) = &doc.assessment {
        out!(
            "{:<11}score {}/10, {} finding(s)",
            "assess",
            a.score,
            a.findings.len()
        );
        for fnd in &a.findings {
            out!(
                "{:<11}{} {}: {}",
                "finding",
                fnd.severity,
                fnd.path,
                fnd.finding
            );
        }
    }
    for d in &doc.deploys {
        let sha = &d.sha[..d.sha.len().min(8)];
        let status = match d.check_ok {
            Some(true) => "ok".to_string(),
            Some(false) => match &d.rolled_back_to {
                Some(to) => format!("rolled back to {}", &to[..to.len().min(8)]),
                None => "failed".to_string(),
            },
            None => "running".to_string(),
        };
        out!(
            "{:<11}{} {sha} {status} {}",
            "deploy",
            d.target,
            render::utc(d.finished_at.unwrap_or(d.started_at))
        );
    }
    for r in &task.refs {
        let label = if r.label.is_empty() {
            String::new()
        } else {
            format!(" ({})", r.label)
        };
        out!("{:<11}{} {}{}", "ref", r.kind, r.url, label);
    }
    for d in &task.decisions {
        out!("decision   {} → {}", d.question, d.answer);
    }
    out!("workflow   {} {}", task.workflow, task.workflow_hash);
    if !task.interface.is_empty() {
        out!(
            "interface  {}",
            task.interface.lines().collect::<Vec<_>>().join(" / ")
        );
    }
    if !task.plan.is_empty() {
        let label = if task.workflow == "intake" {
            "brief"
        } else {
            "plan"
        };
        out!(
            "{:<11}{}",
            label,
            task.plan.lines().collect::<Vec<_>>().join(" / ")
        );
    }
    out!("text       {}", task.text);
    for a in &doc.attempts {
        out!();
        out!(
            "attempt {} [{}]  {}{}  {}  {} turns  {} tools  {:.1}s  {}  {} commit(s)  {} file(s){}",
            a.attempt_no,
            a.step,
            a.state,
            if a.reason.is_empty() {
                String::new()
            } else {
                format!(" ({})", a.reason)
            },
            if a.timed_out {
                "TIMED OUT".to_string()
            } else {
                format!(
                    "exit {}",
                    a.agent_exit.map_or("-".into(), |v| v.to_string())
                )
            },
            a.num_turns,
            a.tool_calls,
            a.agent_ms as f64 / 1000.0,
            a.cost_usd.map_or("-".into(), |c| format!("${c:.4}")),
            a.commits,
            a.files_changed,
            if a.dirty { "  DIRTY" } else { "" }
        );
        out!("  log     {}", a.log_path);
        out!("  agent   runner={} provider={}", a.runner, a.provider);
        if let Ok(o) = serde_json::from_value::<audit::Outputs>(a.outputs.clone())
            && let Some(t) = o.tools
        {
            out!("  ran     {}", t.line());
        }
        if let Ok(checks) =
            serde_json::from_value::<Vec<crate::checks::CheckResult>>(a.verdict.clone())
        {
            for c in checks {
                out!(
                    "  {} {} {} ({:.1}s){}",
                    if c.ok { "✓" } else { "✗" },
                    c.level,
                    c.name,
                    c.ms as f64 / 1000.0,
                    if c.failing_tests.is_empty() {
                        String::new()
                    } else {
                        format!("  failing: {}", c.failing_tests.join(", "))
                    }
                );
            }
        }
        let envelope: Option<crate::envelope::Envelope> = if a.envelope.is_null() {
            None
        } else {
            serde_json::from_value(a.envelope.clone()).ok()
        };
        if let Some(e) = envelope {
            out!(
                "  reported {} change(s), {} check(s) run, {} claim(s)",
                e.changes.len(),
                e.checks_run.len(),
                e.claims.len()
            );
            for c in &e.claims {
                out!("    claim   {} [{}]", c.claim, c.evidence);
            }
            if let Some(q) = &e.needs_input {
                out!("    QUESTION {}", q.question);
            }
        }
        if a.rate_limits.five_hour.is_some() || a.rate_limits.seven_day.is_some() {
            out!(
                "  usage   5h {} · 7d {}",
                a.rate_limits
                    .five_hour
                    .map_or("-".into(), |u| format!("{:.0}%", u * 100.0)),
                a.rate_limits
                    .seven_day
                    .map_or("-".into(), |u| format!("{:.0}%", u * 100.0))
            );
        }
        if !a.result_text.is_empty() {
            let first: String = a
                .result_text
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join(" / ");
            out!("  result  {}", first.chars().take(200).collect::<String>());
        }
    }
    for dgn in &doc.diagnosis {
        out!();
        out!("what       {}", dgn.what);
        out!("action     {}", dgn.action);
    }
    Ok(())
}
