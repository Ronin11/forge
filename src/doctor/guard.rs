//! The `guard` and `guard_overrides` rows: whether each project's bare
//! origin has the landing guard hook (`crate::guard`), and the guard's
//! emergency overrides recorded in the last 24 hours.

use super::{Check, Status, check};
use crate::store::Store;

/// Every project with a repository whose bare origin exists on this
/// machine but lacks the landing guard hook: a hand push straight to the
/// base branch would go through unnoticed there.
pub(super) fn check_guard(store: &Store) -> Vec<Check> {
    let projects = match store.list_projects() {
        Ok(p) => p,
        Err(e) => return vec![check("guard", Status::Fail, format!("{e:#}"), "")],
    };
    let mut missing = Vec::new();
    for p in &projects {
        let repos = store.project_repos(&p.name).unwrap_or_default();
        let unguarded = repos.iter().any(|r| {
            crate::guard::bare_origin_sync(std::path::Path::new(&r.repo))
                .is_some_and(|bare| !crate::guard::installed(&bare))
        });
        if unguarded {
            missing.push(p.name.clone());
        }
    }
    vec![if missing.is_empty() {
        check(
            "guard",
            Status::Ok,
            "every project's bare origin rejects a hand push to its base branch",
            "",
        )
    } else {
        check(
            "guard",
            Status::Warn,
            format!(
                "{} project(s) have a bare origin with no landing guard: {}",
                missing.len(),
                missing.join(", ")
            ),
            "forge project guard <name>",
        )
    }]
}

/// The landing guard's emergency overrides (`forge-override=<reason>`,
/// `Store::insert_override_decision`) recorded in the last 24 hours: a
/// hand push that bypassed the integrator, reported loudly rather than
/// silently accepted.
pub(super) fn check_guard_overrides(store: &Store) -> Vec<Check> {
    let since = crate::unix_now() - 24 * 3600;
    let overrides = match store.decisions_of_kind_since("forge-override", since) {
        Ok(d) => d,
        Err(e) => return vec![check("guard_overrides", Status::Fail, format!("{e:#}"), "")],
    };
    vec![if overrides.is_empty() {
        check("guard_overrides", Status::Ok, "none in the last 24h", "")
    } else {
        let lines: Vec<String> = overrides
            .iter()
            .map(|d| {
                format!(
                    "{} by {} ({}): {}",
                    d.repo, d.answered_by, d.question, d.answer
                )
            })
            .collect();
        check(
            "guard_overrides",
            Status::Warn,
            format!(
                "{} override(s) in the last 24h: {}",
                overrides.len(),
                lines.join("; ")
            ),
            "confirm each override was warranted (forge decisions --grep override)",
        )
    }]
}
