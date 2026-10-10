//! Error classification and the operation row every step of a run leaves on
//! the record.

use super::*;

pub enum Fault {
    Task(anyhow::Error),
    Env(anyhow::Error),
}

impl From<Fault> for anyhow::Error {
    fn from(f: Fault) -> Self {
        match f {
            Fault::Task(e) | Fault::Env(e) => e,
        }
    }
}

pub trait Classify<T> {
    #[track_caller]
    fn task(self) -> Result<T, Fault>;
    #[track_caller]
    fn env(self) -> Result<T, Fault>;
}

impl<T, E: Into<anyhow::Error>> Classify<T> for Result<T, E> {
    #[track_caller]
    fn task(self) -> Result<T, Fault> {
        let at = std::panic::Location::caller();
        self.map_err(|e| {
            let error = located(e.into(), at);
            if error.downcast_ref::<crate::egress::SocketError>().is_some() {
                Fault::Env(error)
            } else {
                Fault::Task(error)
            }
        })
    }
    #[track_caller]
    fn env(self) -> Result<T, Fault> {
        let at = std::panic::Location::caller();
        self.map_err(|e| Fault::Env(located(e.into(), at)))
    }
}

/// A bare OS error ("Permission denied (os error 13)") names neither a
/// path nor an operation; the classifying call site is then the one thing
/// known about where it came from, so it is added. One such fault exited
/// the worker after some thirty tasks before its source was found.
fn located(e: anyhow::Error, at: &std::panic::Location<'_>) -> anyhow::Error {
    if e.chain().count() == 1 && e.downcast_ref::<std::io::Error>().is_some() {
        e.context(format!("at {}:{}", at.file(), at.line()))
    } else {
        e
    }
}

/// When an operation started: unix seconds for the row, an `Instant` for
/// the elapsed time, taken together so they always agree.
pub(crate) struct Timer {
    pub(crate) started_at: i64,
    pub(crate) start: Instant,
}

impl Timer {
    pub(crate) fn now() -> Self {
        Self {
            started_at: unix_now(),
            start: Instant::now(),
        }
    }
}

/// One operation row, kernel or user; `task_id` stays a separate parameter
/// of `op` since it is never part of the row's own identity.
pub(crate) struct OpRow<'a> {
    pub(crate) seq: i64,
    pub(crate) name: &'a str,
    pub(crate) kernel: bool,
    pub(crate) ok: bool,
    pub(crate) exit: Option<i32>,
    pub(crate) detail: &'a str,
    pub(crate) attempt_id: Option<i64>,
    pub(crate) output: &'a str,
}

/// Record one operation row, kernel or user.
pub(crate) fn op(f: &Forge, task_id: i64, timer: &Timer, row: OpRow) -> Result<(), Fault> {
    f.store
        .insert_op(&Op {
            task_id,
            seq: row.seq,
            name: row.name.into(),
            kernel: row.kernel,
            started_at: timer.started_at,
            ms: timer.start.elapsed().as_millis() as i64,
            ok: row.ok,
            exit: row.exit,
            detail: row.detail.into(),
            attempt_id: row.attempt_id,
            output: row.output.into(),
            ..Default::default()
        })
        .env()?;
    f.report.emit(
        task_id,
        Event::Op {
            name: row.name,
            kernel: row.kernel,
            ok: row.ok,
            ms: timer.start.elapsed().as_millis(),
            detail: row.detail,
        },
    );
    Ok(())
}
