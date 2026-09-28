//! The worker row while a handoff is in flight (docs/OPS.md, "The running
//! binary").

use super::{Check, Status, check};
use crate::ctx::Paths;
use crate::store::Store;
use crate::worker;

/// A successor claiming while an older worker drains: the release staged,
/// the successor's pid, what the old one still holds, and how the machine's
/// slots divide between them.
pub(super) fn check_succession(paths: &Paths, store: &Store) -> Option<Check> {
    let live = store.live_workers(worker::pid_alive).ok()?;
    let newest = live.last()?;
    let old: Vec<_> = live
        .iter()
        .filter(|w| w.version != newest.version)
        .collect();
    let first = old.first()?;
    let held = |ws: &mut dyn Iterator<Item = &crate::store::WorkerRow>| -> i64 {
        ws.filter_map(|w| store.held_by_worker(w.pid).ok()).sum()
    };
    let draining = held(&mut old.iter().copied());
    let claiming = held(&mut live.iter().filter(|w| w.version == newest.version));
    let slots = live.iter().map(|w| w.slots).max().unwrap_or(0);
    let staged = crate::release::pointed_at(&crate::release::root(&paths.home), "staged")
        .unwrap_or_else(|| newest.version.clone());
    let (status, share, fix) = slot_share(draining, claiming, slots);
    Some(check(
        "worker",
        status,
        format!(
            "release {staged} staged; successor pid {} claiming; {draining} attempts draining on {}{share}",
            newest.pid, first.version
        ),
        fix,
    ))
}

/// `; N of M slots: predecessor a, successor b` for a handoff in flight,
/// WARN when the two hold more than the machine's M. Nothing is said when
/// no worker recorded its slot count.
fn slot_share(predecessor: i64, successor: i64, slots: usize) -> (Status, String, &'static str) {
    if slots == 0 {
        return (Status::Ok, String::new(), "");
    }
    let sum = predecessor + successor;
    let share =
        format!("; {sum} of {slots} slots: predecessor {predecessor}, successor {successor}");
    if sum > slots as i64 {
        (
            Status::Warn,
            share,
            "the box runs more attempts than `forge work --jobs` allows until the predecessor drains; a successor claims only the slots the predecessor is not using, so a worker from before that rule is still running",
        )
    } else {
        (Status::Ok, share, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handoff_within_the_slots_says_how_they_divide_and_over_them_warns() {
        let (status, text, _) = slot_share(2, 2, 4);
        assert!(status == Status::Ok);
        assert_eq!(text, "; 4 of 4 slots: predecessor 2, successor 2");
        let (status, text, fix) = slot_share(5, 4, 4);
        assert!(status == Status::Warn, "{text}");
        assert_eq!(text, "; 9 of 4 slots: predecessor 5, successor 4");
        assert!(!fix.is_empty());
        let (status, text, _) = slot_share(1, 0, 0);
        assert!(status == Status::Ok && text.is_empty());
    }
}
