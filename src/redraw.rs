//! The claim-time re-draw of a held arm (docs/ECONOMIST.md, "A held arm is
//! re-drawn at claim time"). `queue::enqueue` draws a task's provider per
//! role from `experiment.toml` (`experiment::extend_explore`) long before
//! the worker claims it; by then the drawn provider's window may be spent
//! while another arm of the same role is free. On 2026-09-28 seven queued
//! tasks had all drawn openai, openai's 5h window was at 100%, and the
//! queue sat idle for over an hour beside anthropic at 29%.
//!
//! `decide` is pure: which arm a candidate runs under given who is held.
//! `worker::route_candidate` feeds it the store's windows and applies the
//! outcome.

use crate::experiment;
use crate::store::Task;
use std::collections::BTreeMap;

/// What the worker does with one queued candidate whose next role is
/// `role`.
#[derive(Debug, PartialEq)]
pub enum Routing {
    /// The provider it runs under is free: claim it as it stands.
    Free,
    /// Its drawn arm is held and another is not: claim it after replacing
    /// its explore draws with `explore`, which carries the re-draw and
    /// `note` (also the event text).
    Redrawn {
        explore: BTreeMap<String, String>,
        note: String,
    },
    /// It waits: its provider is held and no other arm can take it (every
    /// arm is held, or the task's provider is not an experiment arm — an
    /// explicit `--provider`, a project pin, the operator's `[roles]`).
    /// `provider` and `until` are the soonest-freed of the providers that
    /// could have run it.
    Held {
        provider: String,
        msg: String,
        until: i64,
    },
}

/// The key `explore` carries the note of a re-draw of `role` under. Not a
/// role, so `ctx::resolve_provider_routed` never reads it, like the
/// `map`/`tools`/`continuation` draws beside it.
pub fn note_key(role: &str) -> String {
    format!("redraw:{role}")
}

/// `weights` without the levels `held` names, renormalised to sum to 1.0:
/// the experiment's weights among the arms still open. Empty when every
/// arm is held.
pub fn open_arms(
    weights: &BTreeMap<String, f64>,
    held: impl Fn(&str) -> bool,
) -> BTreeMap<String, f64> {
    let open: BTreeMap<String, f64> = weights
        .iter()
        .filter(|(level, w)| **w > 0.0 && !held(level))
        .map(|(level, w)| (level.clone(), *w))
        .collect();
    let total: f64 = open.values().sum();
    if total <= 0.0 {
        return BTreeMap::new();
    }
    open.into_iter().map(|(l, w)| (l, w / total)).collect()
}

/// `HH:MM` (UTC) of a unix second, for the note.
fn clock(at: i64) -> String {
    let s = at.rem_euclid(86_400);
    format!("{:02}:{:02}", s / 3600, s % 3600 / 60)
}

/// Decide how `t`, whose next agent step runs `role` under `provider`,
/// is routed. `weights` is `experiment.toml`'s factor for `role` (`None`
/// when none is declared); `hold` says whether a provider is held, and
/// until when. A task is re-drawn only when its provider is one of that
/// factor's arms *and* is what the task's explore draw says: a task that
/// names its own `--provider`, or whose provider came from anywhere but
/// the experiment, is routed by hand and waits instead. The re-draw is
/// `experiment::draw_level` over the open arms, the task's own draw value
/// (a pure function of its id and the role) against the renormalised
/// weights, so it is reproducible and needs no randomness of its own.
pub fn decide(
    t: &Task,
    role: &str,
    provider: &str,
    weights: Option<&BTreeMap<String, f64>>,
    hold: impl Fn(&str) -> Option<(String, i64)>,
) -> Routing {
    let Some((msg, until)) = hold(provider) else {
        return Routing::Free;
    };
    let held_here = || Routing::Held {
        provider: provider.to_string(),
        msg: msg.clone(),
        until,
    };
    let Some(weights) = weights else {
        return held_here();
    };
    if !t.provider.is_empty()
        || t.explore.get(role).map(String::as_str) != Some(provider)
        || !weights.contains_key(provider)
    {
        return held_here();
    }
    let open = open_arms(weights, |level| hold(level).is_some());
    let Some(to) = experiment::draw_level(t.id, role, &open) else {
        // Every arm is held: it waits for the first to be freed.
        let (soonest, (msg, until)) = weights
            .keys()
            .filter_map(|l| hold(l).map(|h| (l.clone(), h)))
            .min_by_key(|(_, (_, until))| *until)
            .unwrap_or_else(|| (provider.to_string(), (msg.clone(), until)));
        return Routing::Held {
            provider: soonest,
            msg,
            until,
        };
    };
    let note = format!(
        "{role}: {provider} held until {}, re-drawn {to}",
        clock(until)
    );
    let mut explore = t.explore.clone();
    explore.insert(role.to_string(), to);
    let key = note_key(role);
    let noted = match explore.get(&key) {
        Some(earlier) => format!("{earlier}; {note}"),
        None => note.clone(),
    };
    explore.insert(key, noted);
    Routing::Redrawn { explore, note }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weights(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(l, w)| (l.to_string(), *w)).collect()
    }

    fn task(id: i64, role: &str, drawn: &str) -> Task {
        Task {
            id,
            explore: BTreeMap::from([(role.to_string(), drawn.to_string())]),
            ..Default::default()
        }
    }

    /// 02:10 UTC on 2026-09-28.
    const UNTIL: i64 = 1_790_561_400;

    fn holding<'a>(held: &'a [&'a str]) -> impl Fn(&str) -> Option<(String, i64)> + 'a {
        move |p| {
            held.contains(&p)
                .then(|| (format!("{p}: rate window 5h at 100%"), UNTIL))
        }
    }

    #[test]
    fn open_arms_are_renormalised_without_the_held_ones() {
        let w = weights(&[("anthropic", 0.7), ("anthropic-opus", 0.1), ("openai", 0.2)]);
        let open = open_arms(&w, |l| l == "openai");
        assert_eq!(open.len(), 2);
        assert!((open["anthropic"] - 0.875).abs() < 1e-9, "{open:?}");
        assert!((open["anthropic-opus"] - 0.125).abs() < 1e-9, "{open:?}");
        assert!((open.values().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(open_arms(&w, |_| true).is_empty());
        assert_eq!(open_arms(&w, |_| false).len(), 3);
    }

    #[test]
    fn a_free_provider_is_left_alone() {
        let w = weights(&[("anthropic", 0.8), ("openai", 0.2)]);
        let t = task(825, "review", "openai");
        assert_eq!(
            decide(&t, "review", "openai", Some(&w), holding(&["anthropic"])),
            Routing::Free
        );
    }

    #[test]
    fn a_held_arm_is_redrawn_to_the_one_free_arm_with_a_note() {
        let w = weights(&[("anthropic", 0.8), ("openai", 0.2)]);
        let t = task(825, "review", "openai");
        let Routing::Redrawn { explore, note } =
            decide(&t, "review", "openai", Some(&w), holding(&["openai"]))
        else {
            panic!("expected a re-draw");
        };
        assert_eq!(explore["review"], "anthropic");
        assert_eq!(note, "review: openai held until 02:10, re-drawn anthropic");
        assert_eq!(explore["redraw:review"], note);
    }

    #[test]
    fn a_second_redraw_keeps_the_first_in_the_note() {
        let w = weights(&[("a", 0.5), ("b", 0.3), ("c", 0.2)]);
        let mut t = task(7, "code", "b");
        t.explore.insert(
            "redraw:code".into(),
            "code: a held until 01:00, re-drawn b".into(),
        );
        let Routing::Redrawn { explore, .. } =
            decide(&t, "code", "b", Some(&w), holding(&["a", "b"]))
        else {
            panic!("expected a re-draw");
        };
        assert_eq!(explore["code"], "c");
        assert_eq!(
            explore["redraw:code"],
            "code: a held until 01:00, re-drawn b; code: b held until 02:10, re-drawn c"
        );
    }

    /// The re-draw follows the renormalised weights: with the 0.6 arm held,
    /// the 0.3 and 0.1 arms take 3/4 and 1/4 of the tasks.
    #[test]
    fn the_redraw_follows_the_renormalised_weights() {
        let w = weights(&[("a", 0.6), ("b", 0.3), ("c", 0.1)]);
        let (mut b, mut c) = (0, 0);
        for id in 1..=2000 {
            let t = task(id, "review", "a");
            match decide(&t, "review", "a", Some(&w), holding(&["a"])) {
                Routing::Redrawn { explore, .. } => match explore["review"].as_str() {
                    "b" => b += 1,
                    "c" => c += 1,
                    other => panic!("re-drawn to {other}"),
                },
                other => panic!("task {id}: {other:?}"),
            }
        }
        let share = b as f64 / (b + c) as f64;
        assert!((share - 0.75).abs() < 0.04, "b took {share:.3} of {b}+{c}");
    }

    #[test]
    fn the_redraw_is_reproducible() {
        let w = weights(&[("a", 0.6), ("b", 0.3), ("c", 0.1)]);
        let t = task(41, "code", "a");
        assert_eq!(
            decide(&t, "code", "a", Some(&w), holding(&["a"])),
            decide(&t, "code", "a", Some(&w), holding(&["a"]))
        );
    }

    #[test]
    fn a_task_waits_only_when_every_arm_is_held() {
        let w = weights(&[("anthropic", 0.8), ("openai", 0.2)]);
        let t = task(825, "review", "openai");
        let held = |p: &str| match p {
            "openai" => Some(("openai: rate window 5h at 100%".to_string(), UNTIL)),
            "anthropic" => Some(("anthropic: rate window 5h at 95%".to_string(), UNTIL - 600)),
            _ => None,
        };
        assert_eq!(
            decide(&t, "review", "openai", Some(&w), held),
            Routing::Held {
                provider: "anthropic".into(),
                msg: "anthropic: rate window 5h at 95%".into(),
                until: UNTIL - 600,
            }
        );
    }

    #[test]
    fn a_hand_routed_task_is_never_redrawn() {
        let w = weights(&[("anthropic", 0.8), ("openai", 0.2)]);
        let mut named = task(1, "review", "openai");
        named.provider = "openai".into();
        let held = holding(&["openai"]);
        assert!(matches!(
            decide(&named, "review", "openai", Some(&w), &held),
            Routing::Held { .. }
        ));
        // Routed by the operator's [roles], not by a draw.
        let undrawn = Task::default();
        assert!(matches!(
            decide(&undrawn, "review", "openai", Some(&w), &held),
            Routing::Held { .. }
        ));
        // No experiment for the role.
        let t = task(1, "review", "openai");
        assert!(matches!(
            decide(&t, "review", "openai", None, &held),
            Routing::Held { .. }
        ));
    }
}
