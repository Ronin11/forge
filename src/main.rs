//! Forge 2: one binary, no daemon. `forge run <repo> "<task>"` creates a
//! worktree, runs the agent in it under bubblewrap with a wall-clock
//! timeout, verifies the result at three levels, retries with the failure
//! as feedback, pushes on success, and records every attempt. `forge add`
//! queues the same thing and `forge work` drains the queue.

mod agent;
mod assess;
mod attempt;
mod audit;
mod checks;
mod cli;
mod concierge;
mod config;
mod ctx;
mod deploy;
mod deploy_look;
mod doctor;
mod engine;
mod envelope;

mod git;
mod journal;
mod landing;
mod operation;
mod plugins;
mod profile;
mod prompts;
mod queue;
mod report;
mod sandbox;
mod store;
mod supervisor;
mod tools;
mod verify;
mod view;
mod worker;
mod workflows;

use std::time::{SystemTime, UNIX_EPOCH};

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::main().await
}
