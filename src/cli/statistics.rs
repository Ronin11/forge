/// Filters, output modes, and repricing controls for the statistics command.
pub struct StatsOptions {
    pub tools: bool,
    pub step: Option<String>,
    pub quality: bool,
    pub journal: bool,
    pub tests: bool,
    pub last: i64,
    pub by_role: bool,
    pub factors: bool,
    pub questions: bool,
    pub days: Option<i64>,
    pub project: Option<String>,
    pub initiative: Option<i64>,
    pub reprice: bool,
    pub provider: Option<String>,
    pub force: bool,
    pub json: bool,
}

use super::*;

/// Tool usage per step, read from attempts only (`forge stats --tools`
/// and `--json --tools`): a job's directive step runs with `no_tools`
/// (see `job::run_directive`), so unlike `Store::role_stats` this stays
/// out of the `job_steps` union — there is nothing there to count.
fn collect_tool_stats(
    f: &Forge,
    step: Option<&str>,
) -> Result<std::collections::BTreeMap<String, (usize, crate::tools::Tools)>> {
    use std::collections::BTreeMap;
    // step -> aggregated tools
    let mut per_step: BTreeMap<String, (usize, crate::tools::Tools)> = BTreeMap::new();
    for (_task_id, step, outputs_json) in f.store.attempt_tool_facts(step)? {
        let Ok(o) = serde_json::from_str::<audit::Outputs>(&outputs_json) else {
            continue;
        };
        let Some(tools) = o.tools else {
            continue;
        };
        let e = per_step
            .entry(step)
            .or_insert((0, crate::tools::Tools::default()));
        e.0 += 1;
        for (k, u) in tools.by_tool {
            let x = e.1.by_tool.entry(k).or_default();
            x.calls += u.calls;
            x.ms += u.ms;
        }
        for (k, u) in tools.shell {
            let x = e.1.shell.entry(k).or_default();
            x.calls += u.calls;
            x.ms += u.ms;
        }
        for (k, n) in tools.reads {
            *e.1.reads.entry(k).or_default() += n;
        }
    }
    Ok(per_step)
}

fn tool_stats(f: &Forge, step: Option<&str>) -> Result<()> {
    let per_step = collect_tool_stats(f, step)?;
    if per_step.is_empty() {
        out!("no attempts with tool facts yet (recorded from the next attempt on)");
        return Ok(());
    }
    for (step, (n, t)) in per_step {
        out!("{step}  ({n} attempt(s))");
        out!(
            "  {:<14} {:>6} {:>9} {:>9}",
            "TOOL",
            "CALLS",
            "TOTAL s",
            "s/CALL"
        );
        for (name, u) in &t.by_tool {
            out!(
                "  {:<14} {:>6} {:>9.1} {:>9.2}",
                name,
                u.calls,
                u.ms as f64 / 1000.0,
                if u.calls > 0 {
                    u.ms as f64 / 1000.0 / u.calls as f64
                } else {
                    0.0
                }
            );
        }
        let mut shell: Vec<_> = t.shell.iter().collect();
        shell.sort_by(|a, b| b.1.ms.cmp(&a.1.ms));
        if !shell.is_empty() {
            out!(
                "  {:<14} {:>6} {:>9} {:>9}",
                "SHELL",
                "CALLS",
                "TOTAL s",
                "s/CALL"
            );
            for (name, u) in shell.iter().take(12) {
                out!(
                    "  {:<14} {:>6} {:>9.1} {:>9.2}",
                    name,
                    u.calls,
                    u.ms as f64 / 1000.0,
                    if u.calls > 0 {
                        u.ms as f64 / 1000.0 / u.calls as f64
                    } else {
                        0.0
                    }
                );
            }
        }
        let mut reads: Vec<_> = t.reads.iter().collect();
        reads.sort_by(|a, b| b.1.cmp(a.1));
        if !reads.is_empty() {
            out!(
                "  most read: {}",
                reads
                    .iter()
                    .take(8)
                    .map(|(p, n)| format!("{p} ({n})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        out!();
    }
    Ok(())
}

fn tools_json(f: &Forge, step: Option<&str>) -> Result<serde_json::Value> {
    let per_step = collect_tool_stats(f, step)?;
    let mut steps = serde_json::Map::new();
    for (step, (n, t)) in per_step {
        let by_tool: serde_json::Map<String, serde_json::Value> = t
            .by_tool
            .iter()
            .map(|(name, u)| {
                let s = u.ms as f64 / 1000.0;
                (
                    name.clone(),
                    serde_json::json!({
                        "calls": u.calls,
                        "total_s": s,
                        "s_per_call": if u.calls > 0 { s / u.calls as f64 } else { 0.0 },
                    }),
                )
            })
            .collect();
        let shell: serde_json::Map<String, serde_json::Value> = t
            .shell
            .iter()
            .map(|(name, u)| {
                let s = u.ms as f64 / 1000.0;
                (
                    name.clone(),
                    serde_json::json!({
                        "calls": u.calls,
                        "total_s": s,
                        "s_per_call": if u.calls > 0 { s / u.calls as f64 } else { 0.0 },
                    }),
                )
            })
            .collect();
        steps.insert(
            step,
            serde_json::json!({
                "attempts": n,
                "by_tool": by_tool,
                "shell": shell,
                "reads": t.reads,
            }),
        );
    }
    Ok(serde_json::Value::Object(steps))
}

pub(super) async fn stats(args: StatsOptions) -> Result<()> {
    let StatsOptions {
        tools,
        step,
        quality,
        journal,
        tests,
        last,
        by_role,
        factors,
        questions,
        days,
        project,
        initiative,
        reprice,
        provider,
        force,
        json,
    } = args;
    let f = Forge::open(false, false)?;
    if reprice {
        return reprice_stats(&f, provider.as_deref(), force, json);
    }
    if questions {
        return questions_stats(&f, days, json);
    }
    let scope = crate::store::StatsFilter {
        project,
        initiative,
    };
    if json {
        let mut doc = crate::view::stats_doc(&f, &scope, days).await?;
        if tools {
            doc.tools = Some(tools_json(&f, step.as_deref())?);
        }
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    if tools {
        return tool_stats(&f, step.as_deref());
    }
    if quality {
        return quality_stats(&f, &scope).await;
    }
    if journal {
        return journal_control_stats(&f).await;
    }
    if tests {
        return test_run_stats(&f, last);
    }
    if by_role {
        return by_role_stats(&f).await;
    }
    if factors {
        return factor_stats_cmd(&f, &scope, days).await;
    }
    let doc = crate::view::stats_doc(&f, &scope, None).await?;
    out!(
        "{:<8} {:<16} {:>5} {:>4} {:>4} {:>4} {:>4} {:>5} {:>9} {:>9} {:>6} {:>9}",
        "WF",
        "HASH",
        "TASKS",
        "OK",
        "FAIL",
        "BLK",
        "UNV",
        "ATT",
        "COST",
        "$/OK",
        "LANDED",
        "$/LANDED"
    );
    for w in &doc.workflows {
        out!(
            "{:<8} {:<16} {:>5} {:>4} {:>4} {:>4} {:>4} {:>5} {:>9} {:>9} {:>6} {:>9}",
            w.workflow,
            w.hash,
            w.pieces,
            w.succeeded,
            w.failed,
            w.blocked,
            w.unverified,
            w.attempts,
            format!("${:.2}", w.mean_cost_usd),
            match w.cost_per_success_usd {
                Some(c) => format!("${c:.2}"),
                None => "-".into(),
            },
            w.landed,
            match w.cost_per_landed_usd {
                Some(c) => format!("${c:.2}"),
                None => "-".into(),
            }
        );
    }
    out!();
    out!(
        "{:<8} {:<8} {:>5} {:>4} {:>6} {:>6} {:>5} {:>6} {:>6} {:>7} {:>9} {:>9}",
        "WF",
        "STEP",
        "ATT",
        "OK",
        "AGENTF",
        "CHECKF",
        "ASK",
        "TURNS",
        "EDIT@",
        "SECS",
        "COST",
        "TOKENS"
    );
    for st in &doc.steps {
        out!(
            "{:<8} {:<8} {:>5} {:>4} {:>6} {:>6} {:>5} {:>6.1} {:>6} {:>7.0} {:>9} {:>9}",
            st.workflow,
            st.step,
            st.attempts,
            st.succeeded,
            st.agent_failed,
            st.checks_failed,
            st.needs_input,
            st.mean_turns,
            st.mean_first_edit
                .map_or("-".to_string(), |v| format!("{v:.1}")),
            st.mean_secs,
            format!("${:.2}", st.cost_usd),
            st.mean_input_tokens
                .map_or("-".to_string(), |v| format!("{v:.0}"))
        );
    }
    if !doc.projects.is_empty() {
        out!();
        out!(
            "{:<16} {:>5} {:>6} {:>9} {:>7}",
            "PROJECT",
            "TASKS",
            "LANDED",
            "COST",
            "DEFECT%"
        );
        for p in &doc.projects {
            out!(
                "{:<16} {:>5} {:>6} {:>9} {:>7}",
                p.project,
                p.tasks,
                p.landed,
                format!("${:.2}", p.cost_usd),
                match p.broke_base_share {
                    Some(s) => format!("{:.0}%", s * 100.0),
                    None => "-".into(),
                }
            );
        }
    }
    if !doc.jobs.is_empty() {
        out!();
        out!(
            "{:<16} {:>5} {:>4} {:>6} {:>11} {:>7}",
            "PROJECT",
            "TODAY",
            "OK",
            "FAILED",
            "NEEDS_HUMAN",
            "SKIPPED"
        );
        for j in &doc.jobs {
            out!(
                "{:<16} {:>5} {:>4} {:>6} {:>11} {:>7}",
                j.project,
                j.today,
                j.ok,
                j.failed,
                j.needs_human,
                j.skipped
            );
        }
    }
    Ok(())
}

/// `forge stats --questions [--days N] [--json]`: every task that blocked
/// with a question in the window, per kind.
fn questions_stats(f: &Forge, days: Option<i64>, json: bool) -> Result<()> {
    let doc = crate::view::questions_doc(f, days)?;
    if json {
        out!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    let hours = |h: Option<f64>| h.map_or("-".to_string(), |h| format!("{h:.1}"));
    let cost = |c: Option<f64>| c.map_or("-".to_string(), |c| format!("${c:.2}"));
    out!(
        "{:<11} {:>5} {:>5} {:>6} {:>6} {:>6} {:>7} {:>9} {:>8} {:>9}",
        "KIND",
        "COUNT",
        "OPEN",
        "SUPERV",
        "OPER",
        "WITHDR",
        "ASSTATED",
        "MEDWAIT_H",
        "ATT_H",
        "ATT_COST"
    );
    for r in doc.kinds.iter().chain(std::iter::once(&doc.total)) {
        out!(
            "{:<11} {:>5} {:>5} {:>6} {:>6} {:>6} {:>7} {:>9} {:>8.2} {:>9}",
            r.kind,
            r.count,
            r.open,
            r.answered_by_supervisor,
            r.answered_by_operator,
            r.withdrawn,
            r.as_stated,
            hours(r.median_wait_hours),
            r.attention_hours,
            cost(r.attention_cost_usd)
        );
    }
    Ok(())
}

/// `forge stats --reprice [--provider NAME] [--force]` (docs/ECONOMIST.md,
/// "Repricing a free-reporting provider"): sets `cost_usd` from recorded
/// tokens on every attempt whose provider reported no cost, for every
/// provider the operator config gives a nonzero price (`Provider::
/// price_input_per_million`/`price_output_per_million`, the same numbers
/// `agent.rs` uses at launch — see `Store::reprice_attempts`), reports how
/// many rows changed and their total, and records the run as a decision
/// row (`Store::insert_reprice_decision`) whether or not anything changed,
/// so a rerun's no-op is on the record too.
fn reprice_stats(f: &Forge, provider: Option<&str>, force: bool, json: bool) -> Result<()> {
    if let Some(p) = provider
        && !f.providers.contains_key(p)
    {
        bail!("unknown provider {p:?}");
    }
    let prices: BTreeMap<String, (f64, f64)> = f
        .providers
        .iter()
        .filter(|(_, p)| p.price_input_per_million > 0.0 || p.price_output_per_million > 0.0)
        .map(|(name, p)| {
            (
                name.clone(),
                (p.price_input_per_million, p.price_output_per_million),
            )
        })
        .collect();
    let result = f.store.reprice_attempts(provider, force, &prices)?;
    let scope = provider.map(|p| format!(" for {p}")).unwrap_or_default();
    let question = format!(
        "forge stats --reprice{scope}{}",
        if force { " --force" } else { "" }
    );
    let answer = format!(
        "repriced {} attempt(s){scope}, total ${:.4}",
        result.changed, result.total_usd
    );
    let decision_id = f.store.insert_reprice_decision(&question, &answer)?;
    if json {
        out!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "changed": result.changed,
                "total_usd": result.total_usd,
                "decision_id": decision_id,
            }))?
        );
        return Ok(());
    }
    out!("{answer} (decision {decision_id})");
    Ok(())
}

/// Defect escape, per workflow: of the tasks that landed, how many broke
/// the next task's base or were later repaired, plus delayed cost
/// (repair cost, true cost per landed piece, and churn).
async fn quality_stats(f: &Forge, scope: &crate::store::StatsFilter) -> Result<()> {
    let doc = crate::view::stats_doc(f, scope, None).await?;
    out!(
        "{:<8} {:<16} {:>6} {:>10} {:>9} {:>8} {:>9} {:>10} {:>10} {:>7}",
        "WF",
        "HASH",
        "LANDED",
        "BROKEBASE",
        "BROKE%",
        "REPAIRED",
        "REPAIR%",
        "REPAIRCOST",
        "TRUECOST",
        "CHURN%"
    );
    let pct = |share: Option<f64>| match share {
        Some(s) => format!("{:.0}%", s * 100.0),
        None => "-".into(),
    };
    let dollar = |v: Option<f64>| v.map_or("-".to_string(), |n| format!("${n:.2}"));
    for w in &doc.workflows {
        out!(
            "{:<8} {:<16} {:>6} {:>10} {:>9} {:>8} {:>9} {:>10} {:>10} {:>7}",
            w.workflow,
            w.hash,
            w.landed,
            w.broke_base,
            pct(w.broke_base_share),
            w.repaired,
            pct(w.repaired_share),
            format!("${:.2}", w.repair_cost_usd),
            dollar(w.true_cost_per_landed_usd),
            pct(w.churn_share)
        );
    }
    if !doc.assessment_correlation.is_empty() {
        let rho = |v: Option<f64>| v.map_or("-".to_string(), |n| format!("{n:.2}"));
        let line = doc
            .assessment_correlation
            .iter()
            .map(|c| format!("score vs {}: rho {} (n={})", c.measure, rho(c.rho), c.n))
            .collect::<Vec<_>>()
            .join("; ");
        out!("{line}");
    }
    let per_landed = |v: Option<f64>| v.map_or("-".to_string(), |n| format!("{n:.2}"));
    let secs = |v: Option<f64>| v.map_or("-".to_string(), |n| format!("{n:.0}s"));
    if !doc.human_attention.is_empty() {
        out!();
        out!(
            "{:<8} {:<16} {:>6} {:>5} {:>5} {:>5} {:>5} {:>7} {:>10}",
            "WF",
            "HASH",
            "LANDED",
            "ANSWER",
            "HAND",
            "WDRAWN",
            "HANDC",
            "EVENTS",
            "EVT/LAND"
        );
        for h in &doc.human_attention {
            out!(
                "{:<8} {:<16} {:>6} {:>5} {:>5} {:>5} {:>5} {:>7} {:>10}",
                h.workflow,
                h.hash,
                h.landed,
                h.operator_answers,
                h.hand_landed,
                h.withdrawals,
                h.hand_commits,
                h.events,
                per_landed(h.events_per_landed)
            );
        }
    }
    if !doc.human_attention_projects.is_empty() {
        out!();
        out!(
            "{:<16} {:>6} {:>5} {:>5} {:>5} {:>5} {:>7} {:>10}",
            "PROJECT",
            "LANDED",
            "ANSWER",
            "HAND",
            "WDRAWN",
            "HANDC",
            "EVENTS",
            "EVT/LAND"
        );
        for h in &doc.human_attention_projects {
            out!(
                "{:<16} {:>6} {:>5} {:>5} {:>5} {:>5} {:>7} {:>10}",
                h.project,
                h.landed,
                h.operator_answers,
                h.hand_landed,
                h.withdrawals,
                h.hand_commits,
                h.events,
                per_landed(h.events_per_landed)
            );
        }
    }
    if !doc.time_to_live.is_empty() {
        out!();
        out!(
            "{:<8} {:<16} {:>5} {:>10} {:>10}",
            "WF",
            "HASH",
            "N",
            "MEDIAN",
            "P90"
        );
        for t in &doc.time_to_live {
            out!(
                "{:<8} {:<16} {:>5} {:>10} {:>10}",
                t.workflow,
                t.hash,
                t.n,
                secs(t.median_secs),
                secs(t.p90_secs)
            );
        }
    }
    if !doc.time_to_live_projects.is_empty() {
        out!();
        out!(
            "{:<16} {:>5} {:>10} {:>10}",
            "PROJECT",
            "N",
            "MEDIAN",
            "P90"
        );
        for t in &doc.time_to_live_projects {
            out!(
                "{:<16} {:>5} {:>10} {:>10}",
                t.project,
                t.n,
                secs(t.median_secs),
                secs(t.p90_secs)
            );
        }
    }
    Ok(())
}

/// Attempts, outcomes, cost and wall time per (role, provider, model),
/// role being the attempt's step; landed, broke-base and delayed-cost
/// columns for the `code` role only.
/// Means of the test-run facts per role over the last `last` attempts,
/// counting only attempts that recorded them (docs/CONTEXT.md, B4).
fn test_run_stats(f: &Forge, last: i64) -> Result<()> {
    use std::collections::BTreeMap;
    let mut per: BTreeMap<String, (u64, crate::tools::testruns::TestRuns)> = BTreeMap::new();
    for (step, outputs_json) in f.store.recent_attempt_outputs(last.max(1))? {
        let Some(t) = serde_json::from_str::<audit::Outputs>(&outputs_json)
            .ok()
            .and_then(|o| o.tools)
            .and_then(|t| t.tests)
        else {
            continue;
        };
        let e = per.entry(step).or_default();
        e.0 += 1;
        e.1.forge_test_calls += t.forge_test_calls;
        e.1.cache_hits += t.cache_hits;
        e.1.raw_commands += t.raw_commands;
        e.1.full_suite_runs += t.full_suite_runs;
        e.1.runs_without_edit += t.runs_without_edit;
        e.1.wall_ms += t.wall_ms;
    }
    if per.is_empty() {
        out!("no attempts with test-run facts yet (recorded from the next attempt on)");
        return Ok(());
    }
    out!(
        "mean per attempt over the last {last} attempts; baseline: 3669 runs, 64% with no edit, 1251 full-suite runs, 11.3 h"
    );
    out!(
        "{:<10} {:>5} {:>8} {:>6} {:>6} {:>6} {:>8} {:>8}",
        "ROLE",
        "ATT",
        "FTEST",
        "HITS",
        "RAW",
        "FULL",
        "NOEDIT",
        "WALL s"
    );
    for (step, (n, t)) in per {
        let m = |v: u64| v as f64 / n as f64;
        out!(
            "{:<10} {:>5} {:>8.2} {:>6.2} {:>6.2} {:>6.2} {:>8.2} {:>8.1}",
            step,
            n,
            m(t.forge_test_calls),
            m(t.cache_hits),
            m(t.raw_commands),
            m(t.full_suite_runs),
            m(t.runs_without_edit),
            m(t.wall_ms) / 1000.0
        );
    }
    Ok(())
}

async fn by_role_stats(f: &Forge) -> Result<()> {
    let doc = crate::view::stats_doc(f, &crate::store::StatsFilter::default(), None).await?;
    out!(
        "{:<10} {:<10} {:<16} {:<9} {:>5} {:>8} {:>6} {:>9} {:>7} {:>6} {:>9} {:>7} {:>10} {:>10} {:>7}",
        "ROLE",
        "PROVIDER",
        "MODEL",
        "KIND",
        "ATT",
        "SUCCEED%",
        "TURNS",
        "COST",
        "SECS",
        "LANDED",
        "BROKEBASE",
        "BROKE%",
        "REPAIRCOST",
        "TRUECOST",
        "CHURN%"
    );
    let pct = |share: Option<f64>| match share {
        Some(s) => format!("{:.0}%", s * 100.0),
        None => "-".into(),
    };
    let count = |v: Option<i64>| v.map_or("-".to_string(), |n| n.to_string());
    let dollar = |v: Option<f64>| v.map_or("-".to_string(), |n| format!("${n:.2}"));
    for r in &doc.by_role {
        out!(
            "{:<10} {:<10} {:<16} {:<9} {:>5} {:>8} {:>6.1} {:>9} {:>7.0} {:>6} {:>9} {:>7} {:>10} {:>10} {:>7}",
            r.role,
            r.provider,
            r.model,
            r.kind,
            r.attempts,
            pct(r.succeeded_share),
            r.mean_turns,
            format!("${:.2}", r.mean_cost_usd),
            r.mean_secs,
            count(r.landed),
            count(r.broke_base),
            pct(r.broke_base_share),
            dollar(r.repair_cost_usd),
            dollar(r.true_cost_per_landed_usd),
            pct(r.churn_share)
        );
    }
    if doc
        .by_role
        .iter()
        .any(|r| r.role == "investigate" || r.role == "interview")
    {
        out!(
            "* investigate/interview: an attempt that ended needs_input with a question counts as a success"
        );
    }
    Ok(())
}

/// Landing rate and true cost per factor level, and each level's effect
/// against its factor's reference level from the one joint main-effects
/// fit of log true cost (see docs/ECONOMIST.md, piece 3, and
/// `crate::store::Store::factor_stats`).
async fn factor_stats_cmd(
    f: &Forge,
    scope: &crate::store::StatsFilter,
    days: Option<i64>,
) -> Result<()> {
    let doc = crate::view::stats_doc(f, scope, days).await?;
    out!(
        "{:<14} {:<10} {:>5} {:>6} {:>18} {:>10} {:>12} {:>8} {:>9} {:>10} {:>10} {:>12} {:>10} {:>8} {:>8}",
        "FACTOR",
        "LEVEL",
        "N",
        "LANDED",
        "RATE (95% CI)",
        "TRUECOST",
        "EFFECT(log$)",
        "SE",
        "FIRSTEDIT",
        "CALLS/TURN",
        "GREP/READ",
        "UNEDIT-CHARS",
        "EDIT-TURNS",
        "OUTLINE",
        "DEF"
    );
    let dollar = |v: Option<f64>| v.map_or("-".to_string(), |n| format!("${n:.2}"));
    for r in &doc.factors {
        let rate = format!(
            "{:.0}% ({:.0}-{:.0}%)",
            r.rate * 100.0,
            r.rate_lo * 100.0,
            r.rate_hi * 100.0
        );
        let effect = if r.is_reference {
            "ref".to_string()
        } else {
            match r.effect {
                Some(e) => format!("{e:+.2}"),
                None => "-".into(),
            }
        };
        let se = r.effect_se.map_or("-".to_string(), |v| format!("{v:.2}"));
        let num = |v: Option<f64>| v.map_or("-".to_string(), |n| format!("{n:.1}"));
        out!(
            "{:<14} {:<10} {:>5} {:>6} {:>18} {:>10} {:>12} {:>8} {:>9} {:>10} {:>10} {:>12} {:>10} {:>8} {:>8}",
            r.factor,
            r.level,
            r.tasks,
            r.landed,
            rate,
            dollar(r.mean_true_cost_usd),
            effect,
            se,
            num(r.mean_first_edit_call),
            num(r.mean_calls_per_turn),
            num(r.mean_grep_then_ranged_read_chains),
            num(r.mean_unedited_read_chars),
            num(r.mean_turns_before_first_edit),
            num(r.mean_outline_calls),
            num(r.mean_def_calls)
        );
    }
    if let Some(d) = days {
        out!("* window: last {d} day(s)");
    }
    Ok(())
}

/// The journal control arm's retrospective split: code attempts after the
/// first (`attempt_no > 1`), by whether they were handed a journal. See
/// docs/LATER.md, "The journal measurement was ill-posed three times".
async fn journal_control_stats(f: &Forge) -> Result<()> {
    let doc = crate::view::stats_doc(f, &crate::store::StatsFilter::default(), None).await?;
    out!(
        "{:<11} {:>5} {:>6} {:>7} {:>10} {:>9}",
        "ARM",
        "ATT",
        "TURNS",
        "EDIT@",
        "SUCCEED%",
        "COST"
    );
    let pct = |share: Option<f64>| match share {
        Some(s) => format!("{:.0}%", s * 100.0),
        None => "-".into(),
    };
    for (arm, row) in [("journal", &doc.journal), ("no journal", &doc.no_journal)] {
        out!(
            "{:<11} {:>5} {:>6.1} {:>7} {:>10} {:>9}",
            arm,
            row.attempts,
            row.mean_turns,
            row.mean_first_edit
                .map_or("-".to_string(), |v| format!("{v:.1}")),
            pct(row.succeeded_share),
            format!("${:.2}", row.mean_cost_usd)
        );
    }
    Ok(())
}
