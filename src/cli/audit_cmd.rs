//! `forge audit`: every task lineage with activity since a time, totaled by
//! outcome, with the dangling tips named (see `crate::lineage`).

use super::*;
use crate::lineage::{self, Outcome};

const DEFAULT_SINCE: &str = "24h";

fn since_secs(since: &Option<String>) -> Result<i64> {
    let secs = crate::workflows::parse_duration(since.as_deref().unwrap_or(DEFAULT_SINCE))
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(unix_now() - secs)
}

/// Every lineage touched since `since_at`: one root's full chain per
/// discovered root, classified at its tip.
pub(crate) fn collect(f: &Forge, since_at: i64) -> Result<Vec<lineage::Lineage>> {
    let mut out = Vec::new();
    for root in f.store.roots_since(since_at)? {
        let rows = f.store.lineage(root)?;
        out.extend(lineage::lineage_of(&rows));
    }
    Ok(out)
}

fn audit(since: Option<String>, json: bool) -> Result<()> {
    let f = Forge::open(false, false)?;
    let since_at = since_secs(&since)?;
    let report = lineage::report(&collect(&f, since_at)?);
    if json {
        out!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    for outcome in Outcome::ALL {
        let t = report.outcomes.get(&outcome).copied().unwrap_or_default();
        out!(
            "{:<12} {:>4} task(s)  ${:.2}",
            outcome.as_str(),
            t.tasks,
            t.cost_usd
        );
    }
    if report.dangling.is_empty() {
        out!("no dangling tips");
    } else {
        out!("dangling tips:");
        for d in &report.dangling {
            out!("  task {}: {}", d.id, d.reason);
        }
    }
    Ok(())
}

pub(super) async fn dispatch(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Audit { since, json } => audit(since, json),
        _ => unreachable!("command routed to the wrong family"),
    }
}
