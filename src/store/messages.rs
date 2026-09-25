//! The message record (see docs/PLUGINS.md): what a contact said and what
//! was said back to them, on a channel, so a rule can ask "has this
//! contact replied since" (a `[skip_if]` command reading `forge message
//! list --json`, docs/JOBS.md) without a channel plugin keeping its own
//! log.

use super::*;

/// Which way a message travelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
        }
    }
}

impl TryFrom<&str> for Direction {
    type Error = std::io::Error;

    fn try_from(s: &str) -> std::result::Result<Self, Self::Error> {
        match s {
            "in" => Ok(Direction::In),
            "out" => Ok(Direction::Out),
            other => Err(std::io::Error::other(format!(
                "unknown message direction {other:?}, expected \"in\" or \"out\""
            ))),
        }
    }
}

/// One message on a channel: inbound from `contact`, or outbound to them.
pub struct Message {
    pub id: i64,
    pub project: String,
    pub channel: String,
    pub contact: String,
    pub direction: Direction,
    pub text: String,
    pub at: i64,
    /// The task this message was about, when there is one (a concierge
    /// exchange, an intake interview's question); `None` otherwise.
    pub task_id: Option<i64>,
}

pub(super) const MESSAGE_COLUMNS: &[&str] = &[
    "id",
    "project",
    "channel",
    "contact",
    "direction",
    "text",
    "at",
    "task_id",
];

fn message_from_row(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get("id")?,
        project: r.get("project")?,
        channel: r.get("channel")?,
        contact: r.get("contact")?,
        direction: conv(
            r,
            "direction",
            Direction::try_from(r.get::<_, String>("direction")?.as_str()),
        )?,
        text: r.get("text")?,
        at: r.get("at")?,
        task_id: r.get("task_id")?,
    })
}

/// What `forge message list` filters a project's messages on.
#[derive(Default, Debug, Clone)]
pub struct MessageFilter {
    pub contact: Option<String>,
    pub since: Option<i64>,
    pub direction: Option<Direction>,
}

impl Store {
    /// Record a message on a channel: `direction` is `In` for one that
    /// came from `contact`, `Out` for one sent to them.
    pub fn insert_message(
        &self,
        project: &str,
        channel: &str,
        contact: &str,
        direction: Direction,
        text: &str,
        task_id: Option<i64>,
    ) -> Result<i64> {
        let c = self.lock();
        c.execute(
            "INSERT INTO messages (project, channel, contact, direction, text, at, task_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                project,
                channel,
                contact,
                direction.as_str(),
                text,
                crate::unix_now(),
                task_id
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// One message by id, or `None`.
    pub fn message(&self, id: i64) -> Result<Option<Message>> {
        let c = self.lock();
        Ok(c.query_row(
            &format!(
                "SELECT {} FROM messages WHERE id = ?1",
                MESSAGE_COLUMNS.join(", ")
            ),
            params![id],
            message_from_row,
        )
        .optional()?)
    }

    /// A project's messages, newest first, narrowed by contact, a minimum
    /// `at`, and/or direction.
    pub fn messages(&self, project: &str, q: &MessageFilter) -> Result<Vec<Message>> {
        let c = self.lock();
        let mut stmt = c.prepare(&format!(
            "SELECT {} FROM messages WHERE project = ?1
               AND (?2 IS NULL OR contact = ?2)
               AND (?3 IS NULL OR at >= ?3)
               AND (?4 IS NULL OR direction = ?4)
             ORDER BY id DESC",
            MESSAGE_COLUMNS.join(", ")
        ))?;
        let rows = stmt.query_map(
            params![
                project,
                q.contact,
                q.since,
                q.direction.map(Direction::as_str)
            ],
            message_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Project;

    fn open_with_project() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store
            .create_project(&Project {
                name: "acme".into(),
                purpose: "p".into(),
                created_at: 1,
                ..Default::default()
            })
            .unwrap();
        (dir, store)
    }

    #[test]
    fn records_and_lists_newest_first() {
        let (_d, s) = open_with_project();
        s.insert_message("acme", "signal", "alice", Direction::In, "hi", None)
            .unwrap();
        s.insert_message("acme", "signal", "alice", Direction::Out, "hello", None)
            .unwrap();
        let rows = s.messages("acme", &MessageFilter::default()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "hello");
        assert_eq!(rows[0].direction, Direction::Out);
        assert_eq!(rows[1].text, "hi");
        assert_eq!(rows[1].direction, Direction::In);
    }

    #[test]
    fn filters_by_contact_since_and_direction() {
        let (_d, s) = open_with_project();
        s.insert_message("acme", "signal", "alice", Direction::In, "a1", None)
            .unwrap();
        s.insert_message("acme", "signal", "bob", Direction::In, "b1", None)
            .unwrap();

        let only_alice = s
            .messages(
                "acme",
                &MessageFilter {
                    contact: Some("alice".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(only_alice.len(), 1);
        assert_eq!(only_alice[0].contact, "alice");

        let out_only = s
            .messages(
                "acme",
                &MessageFilter {
                    direction: Some(Direction::Out),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(out_only.is_empty());

        let future_only = s
            .messages(
                "acme",
                &MessageFilter {
                    since: Some(crate::unix_now() + 1000),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(future_only.is_empty());
    }

    #[test]
    fn unknown_direction_is_an_error() {
        assert!(Direction::try_from("sideways").is_err());
    }
}
