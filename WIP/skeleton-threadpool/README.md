# forge

A priority task queue with a pool of runner threads. Standard library only.

- `Task` — a named unit of work (`FnOnce() + Send`) with a `Priority`.
- `PriorityQueue` — max-heap by priority, FIFO within a priority level.
- `RunnerPool` — N worker threads draining one shared queue; panicking tasks
  are contained, and shutdown/drop drains queued work before joining.

```sh
cargo test
cargo run --bin demo
```
