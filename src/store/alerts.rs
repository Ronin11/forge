//! What reaches the operator unasked (docs/OPS.md, "What reaches you
//! unasked"): a doctor-daily run that failed, a doctor WARN or FAIL row. A
//! row here is a condition still standing, from the run that first saw it
//! until the run that no longer does. Whether it was announced is the
//! message record's to say (channel `alerts`, contact `operator`), so a
//! condition every run sees is sent once and afterwards only rides the
//! daily digest while it stands.
use super::*;

/// The message record's channel for what Forge itself told the operator.
pub const ALERT_CHANNEL: &str = "alerts";

/// One standing condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    /// `doctor-failed`, or `row:<name>:<status>` for a doctor row.
    pub key: String,
    pub project: String,
    /// `doctor_failed` or `doctor_row`.
    pub kind: String,
    pub text: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

fn alert_from_row(r: &Row) -> rusqlite::Result<Alert> {
    Ok(Alert {
        key: r.get("key")?,
        project: r.get("project")?,
        kind: r.get("kind")?,
        text: r.get("text")?,
        first_seen: r.get("first_seen")?,
        last_seen: r.get("last_seen")?,
    })
}

impl Store {
    /// Marks `key` standing as of `now`, keeping its `first_seen` when it
    /// already stood. True when it did not stand before.
    pub fn raise_alert(
        &self,
        project: &str,
        kind: &str,
        key: &str,
        text: &str,
        now: i64,
    ) -> Result<bool> {
        let c = self.lock();
        let existed: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM alerts WHERE key=?1)",
            params![key],
            |r| r.get(0),
        )?;
        c.retry_execute(
            "INSERT INTO alerts (key, project, kind, text, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(key) DO UPDATE SET project=?2, kind=?3, text=?4, last_seen=?5",
            params![key, project, kind, text, now],
        )?;
        Ok(!existed)
    }

    /// Ends a standing condition. True when one stood.
    pub fn clear_alert(&self, key: &str) -> Result<bool> {
        let c = self.lock();
        Ok(c.retry_execute("DELETE FROM alerts WHERE key=?1", params![key])? > 0)
    }

    /// Makes `rows` (`(key, text)`) exactly the standing alerts of `kind`
    /// for `project`: each is raised, and every other one of that kind is
    /// cleared, since the run that reported `rows` no longer sees it.
    pub fn sync_alerts(
        &self,
        project: &str,
        kind: &str,
        rows: &[(String, String)],
        now: i64,
    ) -> Result<()> {
        for (key, text) in rows {
            self.raise_alert(project, kind, key, text, now)?;
        }
        for a in self.alerts(Some(project), Some(kind))? {
            if !rows.iter().any(|(k, _)| *k == a.key) {
                self.clear_alert(&a.key)?;
            }
        }
        Ok(())
    }

    /// Standing alerts, oldest first, narrowed by project and kind.
    pub fn alerts(&self, project: Option<&str>, kind: Option<&str>) -> Result<Vec<Alert>> {
        let c = self.lock();
        let mut st = c.prepare(
            "SELECT * FROM alerts WHERE (?1 IS NULL OR project=?1) AND (?2 IS NULL OR kind=?2)
             ORDER BY first_seen, key",
        )?;
        let rows = st.query_map(params![project, kind], alert_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Standing alerts of `kind` for `project` that no message on the
    /// record announces since they were first seen: the ones still owed a
    /// notification. A condition that cleared and came back is owed one
    /// again, since its `first_seen` is new.
    pub fn unannounced_alerts(&self, project: &str, kind: &str) -> Result<Vec<Alert>> {
        let c = self.lock();
        let mut st = c.prepare(
            "SELECT * FROM alerts a WHERE a.project=?1 AND a.kind=?2 AND NOT EXISTS (
               SELECT 1 FROM messages m WHERE m.project=a.project AND m.channel=?3
               AND m.contact='operator' AND m.direction='out' AND m.at>=a.first_seen
               AND substr(m.text, 1, length(a.key) + 2) = a.key || ': ')
             ORDER BY a.first_seen, a.key",
        )?;
        let rows = st.query_map(params![project, kind, ALERT_CHANNEL], alert_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Puts an announcement of `a` on the message record, which is what
    /// `unannounced_alerts` reads back.
    pub fn record_alert_sent(&self, a: &Alert) -> Result<i64> {
        self.insert_message(InsertMessage {
            project: &a.project,
            channel: ALERT_CHANNEL,
            contact: "operator",
            direction: Direction::Out,
            text: &format!("{}: {}", a.key, a.text),
            task_id: None,
        })
    }

    /// What the daily digest for the day ending at `end` repeats: every
    /// condition still standing that was first seen before `end`, so one
    /// announced today is not repeated until tomorrow's digest.
    pub fn persisting_alerts(&self, end: i64) -> Result<Vec<Alert>> {
        Ok(self
            .alerts(None, None)?
            .into_iter()
            .filter(|a| a.first_seen < end)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db")).unwrap();
        s.create_project(&Project {
            name: "forge".into(),
            purpose: "keeps the lights on".into(),
            ..Default::default()
        })
        .unwrap();
        (dir, s)
    }

    fn rows(r: &[(&str, &str)]) -> Vec<(String, String)> {
        r.iter()
            .map(|(k, t)| (k.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn a_row_every_run_reports_is_announced_once() {
        let (_d, s) = store();
        let seen = rows(&[("row:disk:warn", "disk WARN: 4 GB free")]);
        s.sync_alerts("forge", "doctor_row", &seen, 100).unwrap();
        let owed = s.unannounced_alerts("forge", "doctor_row").unwrap();
        assert_eq!(owed.len(), 1);
        s.record_alert_sent(&owed[0]).unwrap();
        for now in [200, 86_500, 172_900] {
            s.sync_alerts("forge", "doctor_row", &seen, now).unwrap();
            assert!(
                s.unannounced_alerts("forge", "doctor_row")
                    .unwrap()
                    .is_empty(),
                "re-announced at {now}"
            );
        }
        let standing = s.alerts(Some("forge"), None).unwrap();
        assert_eq!(standing[0].first_seen, 100);
        assert_eq!(standing[0].last_seen, 172_900);
    }

    #[test]
    fn a_row_that_clears_and_returns_is_announced_again() {
        let (_d, s) = store();
        let seen = rows(&[("row:disk:warn", "disk WARN: 4 GB free")]);
        s.sync_alerts("forge", "doctor_row", &seen, 100).unwrap();
        s.record_alert_sent(&s.unannounced_alerts("forge", "doctor_row").unwrap()[0])
            .unwrap();
        s.sync_alerts("forge", "doctor_row", &[], 200).unwrap();
        assert!(s.alerts(None, None).unwrap().is_empty());
        // A later first_seen than the old announcement's message.
        s.sync_alerts("forge", "doctor_row", &seen, crate::unix_now() + 10)
            .unwrap();
        assert_eq!(
            s.unannounced_alerts("forge", "doctor_row").unwrap().len(),
            1
        );
    }

    #[test]
    fn the_digest_repeats_only_what_stood_before_its_day_ended() {
        let (_d, s) = store();
        s.sync_alerts(
            "forge",
            "doctor_row",
            &rows(&[
                ("row:disk:warn", "disk WARN: 4 GB free"),
                ("row:logs:warn", "logs WARN: 3 GB"),
            ]),
            100,
        )
        .unwrap();
        // The next day: logs cleared, a new row appeared.
        s.sync_alerts(
            "forge",
            "doctor_row",
            &rows(&[
                ("row:disk:warn", "disk WARN: 3 GB free"),
                ("row:spend:warn", "spend WARN: 90%"),
            ]),
            86_500,
        )
        .unwrap();
        let digest: Vec<String> = s
            .persisting_alerts(86_400)
            .unwrap()
            .into_iter()
            .map(|a| a.text)
            .collect();
        assert_eq!(digest, vec!["disk WARN: 3 GB free".to_string()]);
    }

    #[test]
    fn an_announcement_of_one_key_does_not_cover_a_longer_one() {
        let (_d, s) = store();
        s.sync_alerts(
            "forge",
            "doctor_row",
            &rows(&[("row:disk:warn", "a"), ("row:disk:warn2", "b")]),
            100,
        )
        .unwrap();
        let owed = s.unannounced_alerts("forge", "doctor_row").unwrap();
        s.record_alert_sent(&owed[0]).unwrap();
        let left = s.unannounced_alerts("forge", "doctor_row").unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].key, "row:disk:warn2");
    }
}
