//! End-to-end: the real binary against a throwaway repo, a bare origin, and
//! shell-script agents in tests/fakes that speak stream-json. Sandboxed when
//! bwrap is present.

mod support;

mod concierge;
mod contracts;
mod deploy;
mod deploy_errors;
mod economist;
mod environment;
mod event_cursors;
mod execution;
mod executors;
mod fixtures;
mod fold;
mod graph;
mod init;
mod initiatives;
mod intake;
mod jev;
mod jobs;
mod jobs_schedule_retry;
mod knownfixes;
mod landing;
mod landing_assess;
mod landing_concurrency;
mod landing_effects;
mod landing_fetch;
mod landing_rewind;
mod listing;
mod messages;
mod ops;
mod plugins;
mod presence;
mod providers;
mod provision;
mod questions;
mod refs;
mod reload;
mod resume;
mod shadowing;
mod signal;
mod statusline;
mod successor;
mod supervisor;
mod tdd;
mod terminal;
mod trust;
mod upgrade;
mod verdicts;
mod web;
mod webhooks;
mod worker;
mod workflows;
