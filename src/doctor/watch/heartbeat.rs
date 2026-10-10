//! Watch 6, `plugin_heartbeat`: each enabled plugin's
//! `$FORGE_PLUGIN_STATE/heartbeat` (docs/PLUGINS.md, "Heartbeat"), the
//! time of its last successful inbound poll and outbound send. A plugin
//! whose inbound poll stopped succeeding warns once the poll is ten
//! intervals old: signal's receive failed for days into /dev/null while
//! its process stayed up and `plugins` said running.

use super::{Check, Status, check};
use std::collections::BTreeSet;
use std::path::Path;

/// The file a plugin writes in its state directory.
pub const FILE: &str = "heartbeat";
/// Inbound older than this many poll intervals warns.
pub(super) const POLLS: i64 = 10;

/// One plugin's heartbeat, as written: `key=value` lines.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Heartbeat {
    pub inbound_at: Option<i64>,
    pub outbound_at: Option<i64>,
    pub poll_secs: Option<i64>,
}

pub(super) fn parse(text: &str) -> Heartbeat {
    let mut h = Heartbeat::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().parse::<i64>().ok().filter(|n| *n > 0);
        match k.trim() {
            "inbound_at" => h.inbound_at = v,
            "outbound_at" => h.outbound_at = v,
            "poll_secs" => h.poll_secs = v,
            _ => {}
        }
    }
    h
}

fn age(now: i64, at: Option<i64>) -> String {
    at.map_or("never".to_string(), |t| format!("{}s ago", (now - t).max(0)))
}

pub(super) fn watch(home: &Path, enabled: &BTreeSet<String>, now: i64) -> Check {
    let mut lines = Vec::new();
    let mut stale = Vec::new();
    for name in enabled {
        let path = home.join("plugins-state").join(name).join(FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            lines.push(format!("{name}: no heartbeat"));
            continue;
        };
        let h = parse(&text);
        let line = format!(
            "{name}: inbound {}, outbound {}{}",
            age(now, h.inbound_at),
            age(now, h.outbound_at),
            h.poll_secs
                .map_or(String::new(), |p| format!(" (polls every {p}s)"))
        );
        match (h.inbound_at, h.poll_secs) {
            (Some(at), Some(poll)) if now - at > POLLS * poll => stale.push(line),
            _ => lines.push(line),
        }
    }
    if !stale.is_empty() {
        check(
            "plugin_heartbeat",
            Status::Warn,
            format!(
                "inbound poll older than {POLLS} intervals: {}",
                stale.join("; ")
            ),
            "the plugin runs but its poll fails: forge plugin logs <name>",
        )
    } else if enabled.is_empty() {
        check("plugin_heartbeat", Status::Ok, "no plugin enabled", "")
    } else {
        check("plugin_heartbeat", Status::Ok, lines.join("; "), "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_with(beat: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("plugins-state/signal");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), beat).unwrap();
        d
    }

    fn signal() -> BTreeSet<String> {
        ["signal".to_string()].into()
    }

    #[test]
    fn an_inbound_poll_ten_intervals_old_warns() {
        let d = home_with("inbound_at=1000\noutbound_at=1290\npoll_secs=30\n");
        let c = watch(d.path(), &signal(), 1000 + 301);
        assert_eq!(c.status, Status::Warn, "{}", c.detail);
        assert!(c.detail.contains("signal: inbound 301s ago"), "{}", c.detail);
    }

    #[test]
    fn a_fresh_poll_or_no_heartbeat_is_ok() {
        let d = home_with("inbound_at=1000\npoll_secs=30\n");
        assert_eq!(watch(d.path(), &signal(), 1290).status, Status::Ok);
        let empty = tempfile::tempdir().unwrap();
        let c = watch(empty.path(), &signal(), 1290);
        assert_eq!(c.status, Status::Ok);
        assert!(c.detail.contains("signal: no heartbeat"), "{}", c.detail);
    }
}
