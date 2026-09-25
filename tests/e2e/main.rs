//! End-to-end: the real binary against a throwaway repo, a bare origin, and
//! shell-script agents in tests/fakes that speak stream-json. Sandboxed when
//! bwrap is present.

mod support;

mod concierge;
mod contracts;
mod deploy;
mod economist;
mod environment;
mod execution;
mod fixtures;
mod graph;
mod init;
mod initiatives;
mod intake;
mod jobs;
mod knownfixes;
mod landing;
mod listing;
mod messages;
mod ops;
mod plugins;
mod providers;
mod provision;
mod questions;
mod refs;
mod resume;
mod statusline;
mod supervisor;
mod tdd;
mod trust;
mod upgrade;
mod verdicts;
mod web;
mod webhooks;
mod worker;
mod workflows;
