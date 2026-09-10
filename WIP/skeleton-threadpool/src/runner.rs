//! Runners: worker threads that pull tasks from a shared priority queue.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::queue::PriorityQueue;
use crate::task::{Task, TaskId};

/// Shared state between the pool handle and its runners.
struct Shared {
    queue: Mutex<PriorityQueue>,
    available: Condvar,
    shutdown: Mutex<bool>,
}

impl Shared {
    /// Block until a task is available or shutdown is requested.
    /// Returns `None` once shutdown is set and the queue is drained.
    fn next_task(&self) -> Option<Task> {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if let Some(task) = queue.pop() {
                return Some(task);
            }
            if *self.shutdown.lock().unwrap() {
                return None;
            }
            queue = self.available.wait(queue).unwrap();
        }
    }
}

/// A single worker. Runs on its own thread until the pool shuts down.
struct Runner {
    index: usize,
    shared: Arc<Shared>,
}

impl Runner {
    fn run(self) {
        while let Some(task) = self.shared.next_task() {
            let id = task.id();
            let name = task.name().to_string();
            // Catch panics so one bad task does not kill the runner.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task.run()));
            if result.is_err() {
                eprintln!("runner {} : {id} ({name}) panicked", self.index);
            }
        }
    }
}

/// A fixed-size pool of runners draining one shared priority queue.
///
/// Submit work with [`RunnerPool::submit`]; tasks are dispatched in priority
/// order across all runners. Dropping the pool (or calling
/// [`RunnerPool::shutdown`]) finishes queued work and joins all threads.
pub struct RunnerPool {
    shared: Arc<Shared>,
    handles: Vec<JoinHandle<()>>,
}

impl RunnerPool {
    /// Spawn `size` runner threads. Panics if `size == 0`.
    pub fn new(size: usize) -> Self {
        assert!(size > 0, "RunnerPool needs at least one runner");

        let shared = Arc::new(Shared {
            queue: Mutex::new(PriorityQueue::new()),
            available: Condvar::new(),
            shutdown: Mutex::new(false),
        });

        let handles = (0..size)
            .map(|index| {
                let runner = Runner {
                    index,
                    shared: Arc::clone(&shared),
                };
                thread::Builder::new()
                    .name(format!("runner-{index}"))
                    .spawn(move || runner.run())
                    .expect("failed to spawn runner thread")
            })
            .collect();

        RunnerPool { shared, handles }
    }

    /// Queue a task for execution. Returns its id.
    pub fn submit(&self, task: Task) -> TaskId {
        let id = task.id();
        self.shared.queue.lock().unwrap().push(task);
        self.shared.available.notify_one();
        id
    }

    /// Number of runner threads.
    pub fn size(&self) -> usize {
        self.handles.len()
    }

    /// Tasks waiting to be picked up (not counting those currently running).
    pub fn pending(&self) -> usize {
        self.shared.queue.lock().unwrap().len()
    }

    /// Stop accepting the idle loop, finish queued tasks, and join runners.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        {
            // Hold the queue lock while flipping the flag so a runner can't
            // observe an empty queue, miss the flag, and then wait forever.
            let _queue = self.shared.queue.lock().unwrap();
            *self.shared.shutdown.lock().unwrap() = true;
        }
        self.shared.available.notify_all();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for RunnerPool {
    fn drop(&mut self) {
        if !self.handles.is_empty() {
            self.shutdown_inner();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Priority;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn runs_every_submitted_task() {
        let counter = Arc::new(AtomicUsize::new(0));
        let pool = RunnerPool::new(4);
        for _ in 0..100 {
            let c = counter.clone();
            pool.submit(Task::new("inc", move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
        pool.shutdown();
        assert_eq!(counter.load(Ordering::SeqCst), 100);
    }

    #[test]
    fn single_runner_respects_priority_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let pool = RunnerPool::new(1);

        // Block the single runner so the rest of the submissions queue up.
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        pool.submit(Task::with_priority("gate", Priority::Critical, move || {
            let (lock, cv) = &*g;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        }));
        // Give the runner a moment to pick up the gate task.
        thread::sleep(Duration::from_millis(50));

        for (name, p) in [
            ("low", Priority::Low),
            ("high", Priority::High),
            ("normal", Priority::Normal),
            ("critical", Priority::Critical),
        ] {
            let l = log.clone();
            pool.submit(Task::with_priority(name, p, move || {
                l.lock().unwrap().push(name);
            }));
        }

        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();

        pool.shutdown();
        assert_eq!(
            *log.lock().unwrap(),
            ["critical", "high", "normal", "low"]
        );
    }

    #[test]
    fn panicking_task_does_not_kill_pool() {
        let pool = RunnerPool::new(1);
        pool.submit(Task::new("boom", || panic!("boom")));
        let done = Arc::new(AtomicUsize::new(0));
        let d = done.clone();
        pool.submit(Task::new("after", move || {
            d.store(1, Ordering::SeqCst);
        }));
        pool.shutdown();
        assert_eq!(done.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn drop_drains_queue() {
        let counter = Arc::new(AtomicUsize::new(0));
        {
            let pool = RunnerPool::new(2);
            for _ in 0..20 {
                let c = counter.clone();
                pool.submit(Task::new("inc", move || {
                    c.fetch_add(1, Ordering::SeqCst);
                }));
            }
        }
        assert_eq!(counter.load(Ordering::SeqCst), 20);
    }
}
