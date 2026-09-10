//! Task definitions: a unit of work with a priority and an identity.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique identifier for a task, assigned at construction time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    fn next() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        TaskId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task#{}", self.0)
    }
}

/// Scheduling priority. Higher variants are dequeued first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Priority {
    Low = 0,
    #[default]
    Normal = 1,
    High = 2,
    Critical = 3,
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Critical => "critical",
        };
        f.write_str(s)
    }
}

/// The work a task carries. Boxed so tasks of any shape share one queue.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

/// A schedulable unit of work.
pub struct Task {
    id: TaskId,
    name: String,
    priority: Priority,
    job: Job,
}

impl Task {
    /// Create a task with `Priority::Normal`.
    pub fn new(name: impl Into<String>, job: impl FnOnce() + Send + 'static) -> Self {
        Self::with_priority(name, Priority::Normal, job)
    }

    pub fn with_priority(
        name: impl Into<String>,
        priority: Priority,
        job: impl FnOnce() + Send + 'static,
    ) -> Self {
        Task {
            id: TaskId::next(),
            name: name.into(),
            priority,
            job: Box::new(job),
        }
    }

    pub fn id(&self) -> TaskId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Consume the task and execute its job.
    pub fn run(self) {
        (self.job)()
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("priority", &self.priority)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_increasing() {
        let a = Task::new("a", || {});
        let b = Task::new("b", || {});
        assert!(a.id() < b.id());
    }

    #[test]
    fn priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
        assert_eq!(Priority::default(), Priority::Normal);
    }

    #[test]
    fn run_executes_job() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        Task::new("flag", move || f.store(true, Ordering::SeqCst)).run();
        assert!(flag.load(Ordering::SeqCst));
    }
}
