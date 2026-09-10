//! Small demo: submit a burst of mixed-priority tasks to a pool and watch
//! the dispatch order.

use std::thread;
use std::time::Duration;

use forge::{Priority, RunnerPool, Task};

fn main() {
    let pool = RunnerPool::new(2);

    // Occupy both runners so the remaining submissions stack up in the queue
    // and the priority ordering is visible.
    for i in 0..2 {
        pool.submit(Task::with_priority("warmup", Priority::Critical, move || {
            println!("[{:?}] warmup {i} holding a runner", thread::current().name());
            thread::sleep(Duration::from_millis(100));
        }));
    }
    thread::sleep(Duration::from_millis(20));

    let batch = [
        ("compile", Priority::Normal),
        ("lint", Priority::Low),
        ("hotfix", Priority::Critical),
        ("test", Priority::Normal),
        ("deploy", Priority::High),
        ("docs", Priority::Low),
    ];

    for (name, priority) in batch {
        let id = pool.submit(Task::with_priority(name, priority, move || {
            println!(
                "[{:?}] running {name} ({priority})",
                thread::current().name()
            );
            thread::sleep(Duration::from_millis(30));
        }));
        println!("queued {id}: {name} ({priority})");
    }

    println!("pending: {}", pool.pending());
    pool.shutdown();
    println!("all done");
}
