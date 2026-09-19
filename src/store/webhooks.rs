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
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

pub(super) const WEBHOOK_TOKEN_COLUMNS: &[&str] = &[
    "id",
    "project",
    "name",
    "token_hash",
    "created_at",
    "revoked_at",
];

fn webhook_token_from_row(r: &Row) -> rusqlite::Result<WebhookToken> {
    Ok(WebhookToken {
        id: r.get("id")?,
        project: r.get("project")?,
        name: r.get("name")?,
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
        at: i64,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO webhook_tokens (project, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![project, name, token_hash, at],
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

    /// Whether `token_hash` is an active token on `project`'s webhook
    /// `name`: a token is good for the one hook it was minted for.
    pub fn webhook_token_valid(&self, project: &str, name: &str, token_hash: &str) -> Result<bool> {
        Ok(self
            .lock()
            .query_row(
                "SELECT 1 FROM webhook_tokens WHERE project=?1 AND name=?2 AND token_hash=?3 AND revoked_at IS NULL",
                params![project, name, token_hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
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
        s.create_webhook_token("shop", "orders", "h1", 1).unwrap();
        assert!(s.webhook_token_valid("shop", "orders", "h1").unwrap());
        assert!(!s.webhook_token_valid("shop", "orders", "nope").unwrap());
        assert!(!s.webhook_token_valid("shop", "other", "h1").unwrap());

        assert_eq!(s.revoke_webhook_tokens("shop", "orders", 5).unwrap(), 1);
        assert!(!s.webhook_token_valid("shop", "orders", "h1").unwrap());
        assert_eq!(s.revoke_webhook_tokens("shop", "orders", 6).unwrap(), 0);

        let rows = s.webhook_tokens("shop").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].revoked_at, Some(5));
    }

    #[test]
    fn revoking_one_hook_leaves_another_hooks_token_alone() {
        let (_d, s) = store_with_project();
        s.create_webhook_token("shop", "orders", "h1", 1).unwrap();
        s.create_webhook_token("shop", "refunds", "h2", 2).unwrap();
        s.revoke_webhook_tokens("shop", "orders", 3).unwrap();
        assert!(s.webhook_token_valid("shop", "refunds", "h2").unwrap());
    }
}
