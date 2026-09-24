//! Per-hook webhook tokens (docs/JOBS.md, "Triggers"): what lets a caller
//! outside Forge fire one project's one webhook. Only a token's SHA-256
//! is stored, so the database never holds a usable secret; the token
//! itself is printed once, when `forge project webhook token` mints it.

use super::*;

/// One token minted for a project's webhook, without its hash: nothing
/// that lists tokens has any use for it. `revoked_at` is set by `forge
/// project webhook revoke` and never cleared.
#[derive(Debug, Clone)]
pub struct WebhookToken {
    pub id: i64,
    pub project: String,
    pub name: String,
    /// The trust level (`store::Trust`) a delivery under this token carries.
    pub trust: Trust,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

pub(super) const WEBHOOK_TOKEN_COLUMNS: &[&str] = &[
    "id",
    "project",
    "name",
    "token_hash",
    "trust",
    "created_at",
    "revoked_at",
];

fn webhook_token_from_row(r: &Row) -> rusqlite::Result<WebhookToken> {
    Ok(WebhookToken {
        id: r.get("id")?,
        project: r.get("project")?,
        name: r.get("name")?,
        trust: Trust::try_from(r.get::<_, String>("trust")?.as_str()).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
        })?,
        created_at: r.get("created_at")?,
        revoked_at: r.get("revoked_at")?,
    })
}

impl Store {
    /// Record a token (by hash) for `project`'s webhook `name`.
    pub fn create_webhook_token(
        &self,
        project: &str,
        name: &str,
        token_hash: &str,
        trust: Trust,
        at: i64,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO webhook_tokens (project, name, token_hash, trust, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project, name, token_hash, trust.as_str(), at],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Revoke every active token on `project`'s webhook `name`. Returns
    /// how many were revoked; a second call revokes none.
    pub fn revoke_webhook_tokens(&self, project: &str, name: &str, at: i64) -> Result<usize> {
        Ok(self.lock().execute(
            "UPDATE webhook_tokens SET revoked_at=?3 WHERE project=?1 AND name=?2 AND revoked_at IS NULL",
            params![project, name, at],
        )?)
    }

    /// The trust level of `token_hash` if it is an active token on
    /// `project`'s webhook `name` (a token is good for the one hook it was
    /// minted for), else `None`.
    pub fn webhook_token_trust(
        &self,
        project: &str,
        name: &str,
        token_hash: &str,
    ) -> Result<Option<Trust>> {
        let t: Option<String> = self
            .lock()
            .query_row(
                "SELECT trust FROM webhook_tokens WHERE project=?1 AND name=?2 AND token_hash=?3 AND revoked_at IS NULL",
                params![project, name, token_hash],
                |r| r.get(0),
            )
            .optional()?;
        t.map(|t| Trust::try_from(t.as_str()).map_err(Into::into))
            .transpose()
    }

    /// Record the trust level a job was started at (`forge job fire`).
    pub fn set_job_trust(&self, job_id: i64, trust: Trust) -> Result<()> {
        self.lock().execute(
            "INSERT OR REPLACE INTO job_trust (job_id, trust) VALUES (?1, ?2)",
            params![job_id, trust.as_str()],
        )?;
        Ok(())
    }

    /// The trust a job was started at, if one was recorded.
    pub fn job_trust(&self, job_id: i64) -> Result<Option<Trust>> {
        let t: Option<String> = self
            .lock()
            .query_row(
                "SELECT trust FROM job_trust WHERE job_id=?1",
                params![job_id],
                |r| r.get(0),
            )
            .optional()?;
        t.map(|t| Trust::try_from(t.as_str()).map_err(Into::into))
            .transpose()
    }

    /// A project's webhook tokens, oldest first.
    pub fn webhook_tokens(&self, project: &str) -> Result<Vec<WebhookToken>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM webhook_tokens WHERE project=?1 ORDER BY id",
            WEBHOOK_TOKEN_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(params![project], webhook_token_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_project() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("forge.db")).unwrap();
        s.lock()
            .execute(
                "INSERT INTO projects (name, purpose, created_at) VALUES ('shop', 'p', 1)",
                [],
            )
            .unwrap();
        (dir, s)
    }

    #[test]
    fn a_token_is_valid_for_its_own_hook_only_until_revoked() {
        let (_d, s) = store_with_project();
        s.create_webhook_token("shop", "orders", "h1", Trust::Public, 1)
            .unwrap();
        assert!(
            s.webhook_token_trust("shop", "orders", "h1")
                .unwrap()
                .is_some()
        );
        assert!(
            s.webhook_token_trust("shop", "orders", "nope")
                .unwrap()
                .is_none()
        );
        assert!(
            s.webhook_token_trust("shop", "other", "h1")
                .unwrap()
                .is_none()
        );

        assert_eq!(s.revoke_webhook_tokens("shop", "orders", 5).unwrap(), 1);
        assert!(
            s.webhook_token_trust("shop", "orders", "h1")
                .unwrap()
                .is_none()
        );
        assert_eq!(s.revoke_webhook_tokens("shop", "orders", 6).unwrap(), 0);

        let rows = s.webhook_tokens("shop").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].revoked_at, Some(5));
    }

    #[test]
    fn revoking_one_hook_leaves_another_hooks_token_alone() {
        let (_d, s) = store_with_project();
        s.create_webhook_token("shop", "orders", "h1", Trust::Public, 1)
            .unwrap();
        s.create_webhook_token("shop", "refunds", "h2", Trust::Public, 2)
            .unwrap();
        s.revoke_webhook_tokens("shop", "orders", 3).unwrap();
        assert!(
            s.webhook_token_trust("shop", "refunds", "h2")
                .unwrap()
                .is_some()
        );
    }
}
