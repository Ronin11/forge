//! One operation step of a run: what it is given (the secrets and egress
//! hosts it declares, at operator trust only, docs/JOBS.md), the run
//! itself, the redaction of everything it left behind, and the spend it is
//! charged against the job's `[limits] budget`.

use super::{JobStep, log_lines, record_output};
use crate::checks::CheckResult;
use crate::ctx::Forge;
use crate::store::{JobEffect, Trust};
use crate::workflows::RunStep;
use crate::{operation, secrets, unix_now};
use anyhow::Result;
use std::path::Path;
use std::time::Duration;

/// A step to run, with the job's facts it runs under. `env` is the job's
/// own environment for a step (`step_env`); the step's grant is added to it
/// here, last, so nothing earlier can shadow it.
pub(super) struct OperationStep<'a> {
    pub f: &'a Forge,
    pub job_id: i64,
    pub seq: i64,
    pub step: &'a RunStep,
    pub trust: Trust,
    pub env: Vec<(String, String)>,
    pub repo_checks: &'a std::collections::BTreeMap<String, Vec<String>>,
    pub scratch: &'a Path,
    pub idir: &'a Path,
    pub effect_log: &'a Path,
    pub timeout: Duration,
    pub dry_run: bool,
}

/// What the step left the run: whether it passed, the verdict rows a
/// failure adds, its stdout (redacted) and the dollars it is charged.
pub(super) struct Ran {
    pub ok: bool,
    pub verdict: Vec<CheckResult>,
    pub stdout: String,
    pub charged: f64,
}

/// A step that never got as far as a row of its own: refused or unable to
/// start.
fn failed_before_running(step: &str, why: String) -> Ran {
    Ran {
        ok: false,
        verdict: vec![CheckResult {
            level: "OP".to_string(),
            name: step.to_string(),
            ok: false,
            tail: why,
            ..Default::default()
        }],
        stdout: String::new(),
        charged: 0.0,
    }
}

/// Run one operation step and record its `job_steps` row and effects.
pub(super) async fn run(args: OperationStep<'_>) -> Result<Ran> {
    let OperationStep {
        f,
        job_id,
        seq,
        step,
        trust,
        mut env,
        repo_checks,
        scratch,
        idir,
        effect_log,
        timeout,
        dry_run,
    } = args;
    let action = &step.action;
    let before = log_lines(effect_log).len();
    let grant = match secrets::step_grant(
        &action.name,
        secrets::Declared {
            secrets: &step.secrets,
            egress: &step.egress,
        },
        trust,
        f.trust_policy(trust).egress,
        &f.secrets,
        |var| std::env::var(var).ok(),
    ) {
        Ok(g) => g,
        Err(e) => return Ok(failed_before_running(&action.name, format!("{e:#}"))),
    };
    env.extend(grant.env.iter().cloned());
    let cost_file = budget_env(&mut env, idir, seq, step.budget_usd);
    // Bind only this job's input/output directory alongside its scratch tree.
    if let Some(execution) = f.sandbox.as_ref() {
        execution.set_cache_dir(scratch, idir.to_path_buf());
    }
    let restricted = !step.secrets.is_empty() || !step.egress.is_empty();
    let started_at = unix_now();
    let mut r = match operation::run_job_operation(
        action,
        repo_checks,
        scratch,
        &env,
        timeout,
        restricted.then_some(f.sandbox.as_ref()).flatten(),
        restricted.then_some(&grant.egress),
    )
    .await
    {
        Ok(r) => grant.redactor.result(r),
        Err(e) => {
            let why = grant.redactor.redact(&format!("{e:#}"));
            return Ok(failed_before_running(&action.name, why));
        }
    };
    let spend = spend(&cost_file, step.budget_usd, dry_run);
    if let Some(over) = spend.overspent {
        r.ok = false;
        r.tail = format!(
            "{}\nthe step reported ${over:.4}, over its declared budget_usd of ${:.4}",
            r.tail,
            step.budget_usd.unwrap_or_default()
        );
    }
    let (tail, output_ref) = record_output(idir, &seq.to_string(), &r);
    f.store.append_job_step(&JobStep {
        run: 0,
        id: 0,
        job_id,
        seq,
        action: action.name.clone(),
        kind: "operation".to_string(),
        provider: String::new(),
        model: String::new(),
        cost_usd: Some(spend.charged),
        started_at,
        finished_at: Some(unix_now()),
        exit_code: r.exit,
        output_ref,
        tail: tail.clone(),
        outcome: String::new(),
        probabilities: String::new(),
        node: step.node.clone(),
    })?;
    redact_effect_log(effect_log, &grant.redactor);
    for line in log_lines(effect_log).into_iter().skip(before) {
        let mut parts = line.splitn(3, '\t');
        let (Some(kind), Some(target), Some(summary)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        f.store.append_job_effect(&JobEffect {
            id: 0,
            job_id,
            seq,
            kind: kind.to_string(),
            target: target.to_string(),
            summary: summary.to_string(),
            dry_run,
        })?;
    }
    let ok = r.ok;
    let stdout = r.stdout.clone();
    let verdict = if ok {
        Vec::new()
    } else {
        vec![CheckResult { tail, ..r }]
    };
    Ok(Ran {
        ok,
        verdict,
        stdout,
        charged: spend.charged,
    })
}

fn budget_env(
    env: &mut Vec<(String, String)>,
    idir: &Path,
    seq: i64,
    budget: Option<f64>,
) -> std::path::PathBuf {
    let cost_file = idir.join(format!("step-{seq}.cost"));
    if let Some(b) = budget {
        let _ = std::fs::remove_file(&cost_file);
        env.push(("FORGE_STEP_BUDGET_USD".to_string(), b.to_string()));
        env.push((
            "FORGE_COST_FILE".to_string(),
            cost_file.display().to_string(),
        ));
    }
    cost_file
}

/// What one operation step is charged against the job's `[limits] budget`.
#[derive(Debug, PartialEq)]
struct Spend {
    /// The dollars counted: what the step reported, else what it declared
    /// it may spend (nothing on a dry run, which spends nothing).
    charged: f64,
    /// The reported figure, when it was above the step's declared budget.
    overspent: Option<f64>,
}

/// A step declaring `budget_usd` is handed `FORGE_COST_FILE`, and writes
/// the dollars it actually spent there. A step that reports nothing (or
/// something that is not a non-negative number) is charged what it
/// declared: an unreported spend is never a free one. A step that declares
/// no budget is charged nothing and has nowhere to report.
fn spend(cost_file: &Path, budget: Option<f64>, dry_run: bool) -> Spend {
    let Some(budget) = budget else {
        return Spend {
            charged: 0.0,
            overspent: None,
        };
    };
    let reported = std::fs::read_to_string(cost_file)
        .ok()
        .and_then(|t| t.trim().parse::<f64>().ok())
        .filter(|c| c.is_finite() && *c >= 0.0);
    match reported {
        Some(c) => Spend {
            charged: c,
            overspent: (c > budget).then_some(c),
        },
        None => Spend {
            charged: if dry_run { 0.0 } else { budget },
            overspent: None,
        },
    }
}

/// Rewrite an effect log with every secret's value redacted, so the file an
/// operation wrote never keeps what the rows read back from it do not.
fn redact_effect_log(path: &Path, redactor: &secrets::Redactor) {
    if redactor.is_empty() {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(path) {
        let clean = redactor.redact(&text);
        if clean != text {
            let _ = std::fs::write(path, clean);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cost_file(text: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cost");
        if let Some(t) = text {
            std::fs::write(&p, t).unwrap();
        }
        (d, p)
    }

    #[test]
    fn a_step_declaring_no_budget_is_charged_nothing_whatever_it_writes() {
        let (_d, p) = cost_file(Some("3.5"));
        assert_eq!(spend(&p, None, false).charged, 0.0);
    }

    #[test]
    fn a_reported_cost_is_charged_as_reported() {
        let (_d, p) = cost_file(Some(" 0.42\n"));
        assert_eq!(
            spend(&p, Some(1.0), false),
            Spend {
                charged: 0.42,
                overspent: None
            }
        );
    }

    #[test]
    fn a_reported_cost_over_the_declared_budget_is_flagged() {
        let (_d, p) = cost_file(Some("1.5"));
        assert_eq!(
            spend(&p, Some(1.0), false),
            Spend {
                charged: 1.5,
                overspent: Some(1.5)
            }
        );
    }

    #[test]
    fn an_unreported_or_unreadable_spend_is_charged_the_declared_budget() {
        for text in [None, Some(""), Some("lots"), Some("-1"), Some("NaN")] {
            let (_d, p) = cost_file(text);
            assert_eq!(spend(&p, Some(0.75), false).charged, 0.75, "{text:?}");
        }
    }

    #[test]
    fn a_dry_run_that_reports_nothing_is_charged_nothing() {
        let (_d, p) = cost_file(None);
        assert_eq!(spend(&p, Some(0.75), true).charged, 0.0);
    }

    #[test]
    fn the_effect_log_is_rewritten_without_a_secrets_value() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("effects.log");
        std::fs::write(&log, "row\tbook.csv\tkey=abc-123-xyz\nfile\ta.txt\tok\n").unwrap();
        let r = secrets::Redactor::new(&[secrets::Secret::new("t", "T", "abc-123-xyz")]);
        redact_effect_log(&log, &r);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "row\tbook.csv\tkey=[redacted:t]\nfile\ta.txt\tok\n"
        );
    }
}
