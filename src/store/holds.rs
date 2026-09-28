//! provider_holds and provider_probes: a provider whose agent login was
//! refused, held until a probe answers (see src/login_hold.rs), and every
//! probe made while it was, with what it cost.

use super::*;

/// One held provider: since when, why, and when it was last probed.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginHold {
    pub provider: String,
    pub since: i64,
    pub reason: String,
    pub probed_at: Option<i64>,
}

fn hold_from_row(r: &Row) -> rusqlite::Result<LoginHold> {
    Ok(LoginHold {
        provider: r.get("provider")?,
        since: r.get("since")?,
        reason: r.get("reason")?,
        probed_at: r.get("probed_at")?,
    })
}

impl Store {
    /// Hold `provider` for a refused login. True when this starts the hold;
    /// a provider already held keeps its first `since` and reason.
    pub fn hold_login(&self, provider: &str, reason: &str, now: i64) -> Result<bool> {
        let n = self.lock().retry_execute(
            "INSERT OR IGNORE INTO provider_holds (provider, since, reason) VALUES (?1, ?2, ?3)",
            params![provider, now, reason],
        )?;
        Ok(n == 1)
    }

    pub fn login_hold(&self, provider: &str) -> Result<Option<LoginHold>> {
        Ok(self
            .lock()
            .retry_query_row(
                "SELECT provider, since, reason, probed_at FROM provider_holds WHERE provider=?1",
                params![provider],
                hold_from_row,
            )
            .optional()?)
    }

    pub fn login_holds(&self) -> Result<Vec<LoginHold>> {
        let c = self.lock();
        let mut stmt = c.prepare(
            "SELECT provider, since, reason, probed_at FROM provider_holds ORDER BY provider",
        )?;
        let rows = stmt.query_map([], hold_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The login answered again: true when there was a hold to end.
    pub fn release_login(&self, provider: &str) -> Result<bool> {
        let n = self.lock().retry_execute(
            "DELETE FROM provider_holds WHERE provider=?1",
            params![provider],
        )?;
        Ok(n == 1)
    }

    /// One probe of a held provider's login, whatever it found.
    pub fn record_probe(
        &self,
        provider: &str,
        ok: bool,
        cost_usd: f64,
        detail: &str,
        now: i64,
    ) -> Result<()> {
        let c = self.lock();
        c.retry_execute(
            "INSERT INTO provider_probes (provider, at, ok, cost_usd, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![provider, now, ok as i64, cost_usd, detail],
        )?;
        c.retry_execute(
            "UPDATE provider_holds SET probed_at=?2 WHERE provider=?1",
            params![provider, now],
        )?;
        Ok(())
    }

    /// How many probes `provider` has had since `since`.
    pub fn probes_since(&self, provider: &str, since: i64) -> Result<i64> {
        Ok(self.lock().retry_query_row(
            "SELECT COUNT(*) FROM provider_probes WHERE provider=?1 AND at >= ?2",
            params![provider, since],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_starts_once_keeps_its_first_reason_and_ends_on_release() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        assert!(s.hold_login("anthropic", "expired", 100).unwrap());
        assert!(!s.hold_login("anthropic", "again", 200).unwrap());
        let h = s.login_hold("anthropic").unwrap().unwrap();
        assert_eq!(
            (h.since, h.reason.as_str(), h.probed_at),
            (100, "expired", None)
        );
        s.record_probe("anthropic", false, 0.002, "still expired", 700)
            .unwrap();
        assert_eq!(
            s.login_hold("anthropic").unwrap().unwrap().probed_at,
            Some(700)
        );
        assert_eq!(s.probes_since("anthropic", 0).unwrap(), 1);
        assert!(
            (s.spent_since(0).unwrap() - 0.002).abs() < 1e-9,
            "a probe is spend"
        );
        assert_eq!(s.login_holds().unwrap().len(), 1);
        assert!(s.release_login("anthropic").unwrap());
        assert!(!s.release_login("anthropic").unwrap());
        assert!(s.login_hold("anthropic").unwrap().is_none());
    }
}
