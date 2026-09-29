//! The experiment arms a task is drawn into at enqueue: the journal's
//! control arm and each `[measure] explore` role's provider, both a
//! deterministic draw from the task id alone.

use super::*;

/// The task's journal flag and how it got that value. An explicit
/// `--journal`/`--no-journal` always wins and records `"explicit"`;
/// otherwise a deterministic draw from the task id assigns `"control"`
/// (journal off) with probability `fraction`, else `"treatment"`. See
/// docs/LATER.md, "The journal measurement was ill-posed three times".
pub(super) fn assign_journal_arm(
    id: i64,
    choice: Option<bool>,
    fraction: f64,
) -> (bool, &'static str) {
    match choice {
        Some(on) => (on, "explicit"),
        None if journal_control_draw(id, fraction) => (false, "control"),
        None => (true, "treatment"),
    }
}

/// Whether `id` draws into the control arm at `fraction`: a pure function
/// of the two, so the assignment is reproducible from the id alone and
/// never needs to be persisted separately from the id it came from.
/// `fraction <= 0.0` never draws control; `fraction >= 1.0` always does.
pub(super) fn journal_control_draw(id: i64, fraction: f64) -> bool {
    if fraction <= 0.0 {
        return false;
    }
    // A splitmix64-style finalizer: built to take a small sequential
    // counter (task ids) to well-spread output, unlike a plain multiply.
    let mut x = id as u64;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    let draw = (x % 1_000_000) as f64 / 1_000_000.0;
    draw < fraction
}

/// Which provider each of the operator's `[measure] explore` roles is
/// routed to for this task: each role draws independently, at its own
/// fraction, the same deterministic way as the journal control arm (see
/// `journal_control_draw`); recorded on `Task::explore` so
/// `ctx::resolve_provider` can look it up at every step without redoing
/// the draw. An explicit `--provider` on the request routes every role
/// itself and is never overridden, so it draws nothing at all.
pub(super) fn assign_explore(
    id: i64,
    explicit_provider: bool,
    explore: &BTreeMap<String, config::ExploreRole>,
) -> BTreeMap<String, String> {
    if explicit_provider {
        return BTreeMap::new();
    }
    explore
        .iter()
        .filter(|(_, e)| journal_control_draw(id, e.fraction))
        .map(|(role, e)| (role.clone(), e.provider.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_control_draw_is_a_pure_function_of_id_and_fraction() {
        // Same inputs, called independently, always agree: nothing but
        // (id, fraction) feeds the draw.
        for id in [1, 2, 3, 42, 1_000, 1_000_000] {
            for frac in [0.0, 0.1, 0.3, 0.5, 0.9, 1.0] {
                let a = journal_control_draw(id, frac);
                let b = journal_control_draw(id, frac);
                assert_eq!(a, b, "id {id} fraction {frac} disagreed with itself");
            }
        }
        // Raising the fraction only adds control assignments: each id's
        // draw is fixed and only the threshold moves, so the set of ids
        // assigned control at a lower fraction is a subset of a higher one.
        let ids: Vec<i64> = (1..5_000).collect();
        let lo: Vec<bool> = ids
            .iter()
            .map(|&id| journal_control_draw(id, 0.2))
            .collect();
        let hi: Vec<bool> = ids
            .iter()
            .map(|&id| journal_control_draw(id, 0.6))
            .collect();
        for (l, h) in lo.iter().zip(hi.iter()) {
            assert!(!l || *h, "raising the fraction dropped a control draw");
        }
    }

    #[test]
    fn a_fraction_of_zero_never_assigns_control() {
        for id in 1..10_000 {
            assert!(
                !journal_control_draw(id, 0.0),
                "id {id} drew control at fraction 0.0"
            );
        }
    }

    fn explore_of(
        role: &str,
        provider: &str,
        fraction: f64,
    ) -> BTreeMap<String, config::ExploreRole> {
        [(
            role.to_string(),
            config::ExploreRole {
                provider: provider.to_string(),
                fraction,
            },
        )]
        .into()
    }

    #[test]
    fn assign_explore_is_a_pure_function_of_id_and_fraction() {
        let explore = explore_of("code", "devhome", 0.4);
        for id in [1, 2, 3, 42, 1_000, 1_000_000] {
            let a = assign_explore(id, false, &explore);
            let b = assign_explore(id, false, &explore);
            assert_eq!(a, b, "id {id} disagreed with itself");
        }
    }

    #[test]
    fn assign_explore_at_fraction_zero_never_assigns() {
        let explore = explore_of("code", "devhome", 0.0);
        for id in 1..10_000 {
            assert!(
                assign_explore(id, false, &explore).is_empty(),
                "id {id} drew a provider at fraction 0.0"
            );
        }
    }

    #[test]
    fn assign_explore_never_overrides_an_explicit_provider() {
        let explore = explore_of("code", "devhome", 1.0);
        for id in 1..1_000 {
            assert!(
                assign_explore(id, true, &explore).is_empty(),
                "id {id} drew a provider despite an explicit --provider"
            );
        }
    }

    #[test]
    fn assign_explore_draws_each_role_independently() {
        let mut explore = BTreeMap::new();
        explore.insert(
            "code".to_string(),
            config::ExploreRole {
                provider: "devhome".to_string(),
                fraction: 1.0,
            },
        );
        explore.insert(
            "tests".to_string(),
            config::ExploreRole {
                provider: "openai".to_string(),
                fraction: 0.0,
            },
        );
        let drawn = assign_explore(7, false, &explore);
        assert_eq!(drawn.get("code").map(String::as_str), Some("devhome"));
        assert_eq!(drawn.get("tests"), None);
    }
}
