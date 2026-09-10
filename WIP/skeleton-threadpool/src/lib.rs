//! forge: a priority task queue with a pool of runner threads.
//!
//! ```
//! use forge::{Priority, RunnerPool, Task};
//!
//! let pool = RunnerPool::new(2);
//! pool.submit(Task::with_priority("urgent", Priority::High, || println!("first")));
//! pool.submit(Task::new("later", || println!("second")));
//! pool.shutdown(); // waits for both tasks
//! ```

pub mod queue;
pub mod runner;
pub mod task;

pub use queue::PriorityQueue;
pub use runner::RunnerPool;
pub use task::{Job, Priority, Task, TaskId};
