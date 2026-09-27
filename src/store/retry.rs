//! Retry individual statements, never replay a partly committed batch.
use rusqlite::{Connection, ErrorCode, Params, Row};
use std::time::{Duration, Instant};

fn retry<T>(mut statement: impl FnMut(Duration) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut delay = Duration::from_millis(10);
    loop {
        let result = statement(deadline.saturating_duration_since(Instant::now()));
        let retryable = matches!(
            result.as_ref().err().and_then(|e| e.sqlite_error_code()),
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
        );
        if !retryable || Instant::now() >= deadline {
            return result;
        }
        std::thread::sleep(delay.min(deadline.saturating_duration_since(Instant::now())));
        delay = (delay * 2).min(Duration::from_secs(1));
    }
}

pub(super) trait RetryConnection {
    fn retry_execute<P: Params + Clone>(&self, sql: &str, params: P) -> rusqlite::Result<usize>;
    fn retry_query_row<T, P: Params + Clone, F: FnMut(&Row<'_>) -> rusqlite::Result<T>>(
        &self,
        sql: &str,
        params: P,
        row: F,
    ) -> rusqlite::Result<T>;
}

impl RetryConnection for Connection {
    fn retry_execute<P: Params + Clone>(&self, sql: &str, params: P) -> rusqlite::Result<usize> {
        let result = retry(|remaining| {
            self.busy_timeout(remaining)?;
            self.execute(sql, params.clone())
        });
        self.busy_timeout(Duration::from_secs(60))?;
        result
    }

    fn retry_query_row<T, P: Params + Clone, F: FnMut(&Row<'_>) -> rusqlite::Result<T>>(
        &self,
        sql: &str,
        params: P,
        mut row: F,
    ) -> rusqlite::Result<T> {
        let result = retry(|remaining| {
            self.busy_timeout(remaining)?;
            self.query_row(sql, params.clone(), &mut row)
        });
        self.busy_timeout(Duration::from_secs(60))?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, Task, TaskState};

    #[test]
    fn worker_claim_waits_out_a_six_second_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let store = Store::open(&path).unwrap();
        let id = store
            .insert_task(&Task {
                repo: "r".into(),
                task: "t".into(),
                base_branch: "main".into(),
                ..Default::default()
            })
            .unwrap();
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(6));
            writer.execute_batch("COMMIT").unwrap();
        });
        let start = Instant::now();
        let claimed = store.claim_next(42, &[], |_| false).unwrap().unwrap();
        release.join().unwrap();
        assert!(start.elapsed() >= Duration::from_secs(6));
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.state, TaskState::Running);
        assert!(!store.claim(id, 43).unwrap());
    }

    #[test]
    fn busy_and_locked_retry_but_other_errors_do_not() {
        for code in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_CONSTRAINT,
        ] {
            let mut calls = 0;
            let result = retry(|_| {
                calls += 1;
                if calls == 1 {
                    Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(code),
                        None,
                    ))
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.is_ok(), code != rusqlite::ffi::SQLITE_CONSTRAINT);
            assert_eq!(calls, if result.is_ok() { 2 } else { 1 });
        }
    }
}
