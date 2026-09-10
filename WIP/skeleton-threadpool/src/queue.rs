//! A priority queue of tasks.
//!
//! Ordering is by `Priority` first (highest out first), then by insertion
//! order so that tasks of equal priority are served FIFO.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::task::{Priority, Task};

/// Heap entry: wraps a task with a sequence number for stable ordering.
struct Entry {
    seq: u64,
    task: Task,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority wins. On a tie, the *lower* sequence number wins
        // (earlier insertion), so reverse the seq comparison for a max-heap.
        self.task
            .priority()
            .cmp(&other.task.priority())
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

/// A single-threaded priority queue. Wrap in a mutex for sharing; see
/// `runner::RunnerPool` for a ready-made worker pool built on top of this.
#[derive(Default)]
pub struct PriorityQueue {
    heap: BinaryHeap<Entry>,
    next_seq: u64,
}

impl PriorityQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a task to the queue.
    pub fn push(&mut self, task: Task) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.heap.push(Entry { seq, task });
    }

    /// Remove and return the highest-priority task, if any.
    pub fn pop(&mut self) -> Option<Task> {
        self.heap.pop().map(|e| e.task)
    }

    /// Peek at the highest-priority task without removing it.
    pub fn peek(&self) -> Option<&Task> {
        self.heap.peek().map(|e| &e.task)
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Number of queued tasks at exactly `priority`.
    pub fn count_at(&self, priority: Priority) -> usize {
        self.heap
            .iter()
            .filter(|e| e.task.priority() == priority)
            .count()
    }

    /// Drop every queued task without running it.
    pub fn clear(&mut self) {
        self.heap.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(name: &str, p: Priority) -> Task {
        Task::with_priority(name, p, || {})
    }

    #[test]
    fn pops_highest_priority_first() {
        let mut q = PriorityQueue::new();
        q.push(task("low", Priority::Low));
        q.push(task("critical", Priority::Critical));
        q.push(task("normal", Priority::Normal));
        q.push(task("high", Priority::High));

        let order: Vec<_> = std::iter::from_fn(|| q.pop())
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(order, ["critical", "high", "normal", "low"]);
    }

    #[test]
    fn equal_priority_is_fifo() {
        let mut q = PriorityQueue::new();
        for i in 0..5 {
            q.push(task(&format!("t{i}"), Priority::Normal));
        }
        let order: Vec<_> = std::iter::from_fn(|| q.pop())
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(order, ["t0", "t1", "t2", "t3", "t4"]);
    }

    #[test]
    fn fifo_survives_interleaved_priorities() {
        let mut q = PriorityQueue::new();
        q.push(task("n1", Priority::Normal));
        q.push(task("h1", Priority::High));
        q.push(task("n2", Priority::Normal));
        q.push(task("h2", Priority::High));

        let order: Vec<_> = std::iter::from_fn(|| q.pop())
            .map(|t| t.name().to_string())
            .collect();
        assert_eq!(order, ["h1", "h2", "n1", "n2"]);
    }

    #[test]
    fn peek_len_and_clear() {
        let mut q = PriorityQueue::new();
        assert!(q.is_empty());
        assert!(q.peek().is_none());

        q.push(task("a", Priority::Low));
        q.push(task("b", Priority::High));
        assert_eq!(q.len(), 2);
        assert_eq!(q.peek().unwrap().name(), "b");
        assert_eq!(q.count_at(Priority::Low), 1);
        assert_eq!(q.count_at(Priority::Critical), 0);

        q.clear();
        assert!(q.is_empty());
    }
}
