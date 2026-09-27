//! The `presence` row: the presence plugin's own CPU-share state
//! (docs/PLUGINS.md, "presence"), read from the file it writes with each
//! transition into its own `FORGE_PLUGIN_STATE`. Nothing here decides the
//! state; it only reports what the plugin already decided.

use super::{Check, Status, check};
use crate::ctx::Paths;
use crate::store::Store;

struct PresenceState {
    state: String,
    since: i64,
    weight: i64,
}

/// `presence.sh`'s own `state=<...>\nsince=<...>\nweight=<...>` lines. A
/// line this does not recognize, or one of the three missing, is simply
/// not part of the result; a truncated write (a crash mid-write, though
/// `apply` writes through a temp file and renames) reads as unreadable
/// rather than as a wrong-but-parseable state.
fn parse_state(text: &str) -> Option<PresenceState> {
    let mut state = None;
    let mut since = None;
    let mut weight = None;
    for line in text.lines() {
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        match key {
            "state" => state = Some(val.to_string()),
            "since" => since = val.parse().ok(),
            "weight" => weight = val.parse().ok(),
            _ => {}
        }
    }
    Some(PresenceState {
        state: state?,
        since: since?,
        weight: weight?,
    })
}

/// `epoch` as local wall-clock `HH:MM`. Every other check speaks UTC only
/// (`render::utc`) because a client viewing it may be anywhere; this row
/// only ever means something on the desktop presence.sh runs on, the same
/// machine `forge doctor` is being read on, so its own local zone (via
/// libc, the same way `agent::local_time_on` reads it) is the useful one.
fn local_hhmm(epoch: i64) -> String {
    // SAFETY: localtime_r writes only into the tm this call owns.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = epoch as libc::time_t;
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

fn presence_detail(s: &PresenceState) -> String {
    match s.state.as_str() {
        "none" => format!(
            "no presence source: Forge always runs at weight {}",
            s.weight
        ),
        other => format!("{other} since {}, weight {}", local_hhmm(s.since), s.weight),
    }
}

/// Only present when the `presence` plugin is enabled: an operator who
/// never installed it should see nothing, the same posture as every other
/// plugin-specific row would take.
pub(super) fn check_presence(paths: &Paths, store: &Store) -> Vec<Check> {
    let Ok(enabled) = store.enabled_plugins() else {
        return Vec::new();
    };
    if !enabled.contains("presence") {
        return Vec::new();
    }
    let path = paths
        .home
        .join("plugins-state")
        .join("presence")
        .join("state");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return vec![check(
            "presence",
            Status::Warn,
            "enabled but has not written a state yet",
            "give the worker a moment to start it; forge plugin logs presence for its output",
        )];
    };
    let Some(s) = parse_state(&text) else {
        return vec![check(
            "presence",
            Status::Warn,
            format!("{}: unreadable state", path.display()),
            "forge plugin logs presence for what it last wrote",
        )];
    };
    vec![check("presence", Status::Ok, presence_detail(&s), "")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_detail_names_the_state_the_local_time_and_the_weight() {
        let s = PresenceState {
            state: "active".into(),
            since: 1_789_974_000, // 2026-09-21T07:00:00Z
            weight: 40,
        };
        let detail = presence_detail(&s);
        assert!(detail.starts_with("active since "), "{detail}");
        assert!(detail.ends_with(", weight 40"), "{detail}");
    }

    #[test]
    fn presence_detail_for_no_source_names_the_constant_weight() {
        let s = PresenceState {
            state: "none".into(),
            since: 0,
            weight: 100,
        };
        assert_eq!(
            presence_detail(&s),
            "no presence source: Forge always runs at weight 100"
        );
    }

    #[test]
    fn parse_state_reads_the_three_keys_and_ignores_the_rest() {
        let s = parse_state("state=idle\nsince=123\nweight=100\ngarbage\n").unwrap();
        assert_eq!(s.state, "idle");
        assert_eq!(s.since, 123);
        assert_eq!(s.weight, 100);
    }

    #[test]
    fn parse_state_is_none_when_a_key_is_missing() {
        assert!(parse_state("state=idle\nweight=100\n").is_none());
    }

    fn fixture() -> (tempfile::TempDir, Paths, Store) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let paths = Paths {
            worktrees: home.join("worktrees"),
            logs: home.join("logs"),
            home,
        };
        std::fs::create_dir_all(&paths.worktrees).unwrap();
        std::fs::create_dir_all(&paths.logs).unwrap();
        let store = Store::open(&paths.home.join("forge.db")).unwrap();
        (dir, paths, store)
    }

    #[test]
    fn no_row_when_presence_is_not_enabled() {
        let (_dir, paths, store) = fixture();
        assert!(check_presence(&paths, &store).is_empty());
    }

    #[test]
    fn warns_when_enabled_but_no_state_written_yet() {
        let (_dir, paths, store) = fixture();
        store.set_plugin_enabled("presence", true, 1).unwrap();
        let checks = check_presence(&paths, &store);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].status == Status::Warn);
        assert!(checks[0].detail.contains("has not written a state"));
    }

    #[test]
    fn reports_the_state_the_plugin_wrote() {
        let (_dir, paths, store) = fixture();
        store.set_plugin_enabled("presence", true, 1).unwrap();
        let state_dir = paths.home.join("plugins-state").join("presence");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("state"), "state=idle\nsince=1\nweight=100\n").unwrap();
        let checks = check_presence(&paths, &store);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].status == Status::Ok);
        assert!(
            checks[0].detail.starts_with("idle since "),
            "{}",
            checks[0].detail
        );
        assert!(
            checks[0].detail.ends_with(", weight 100"),
            "{}",
            checks[0].detail
        );
    }
}
