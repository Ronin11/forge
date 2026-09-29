//! Workflows and actions are data: one TOML file each under
//! `<FORGE_HOME>/workflows/` (workflows) and `.../workflows/actions/`
//! (actions). An action is a directive (an LLM step) or an operation (a
//! deterministic step). A workflow is an ordered list of references to
//! actions or to other workflows, spliced inline.
//!
//! Identity is the git blob hash of the file. Latest by default: a task
//! resolves everything once at creation, records every hash and every
//! file's text, and runs from that record, so an edit landing mid-run
//! cannot change a running task, and reverting is git. `check` is the one
//! definition of validity; the engine refuses what it rejects.
//! See docs/ACTIONS.md and docs/WORKFLOWS.md.

use anyhow::{Context, Result, bail};
use croner::Cron;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

mod catalog;
mod contract;
mod definitions;
pub mod draft;
pub mod edges;
mod judgment;
mod library;
pub mod lint;
mod project;
mod resolve;
pub mod shadow;
mod trigger;
mod validate;
use catalog::{
    BUILTIN_ACTIONS, BUILTIN_OPERATIONS, blob_hash, builtin_actions_map, dir_of, ensure_sound,
    toml_files, toml_files_if_present,
};
pub(crate) use catalog::{BUILTIN_WORKFLOWS, builtin_action};
pub use catalog::{
    Catalog, catalog_dir, commit_for, declared_name, get, load_actions, load_all, load_catalog,
    uncommitted,
};
pub use contract::{Contract, Kind, OPERATION_PRODUCES, Product, WorkflowKind};
#[allow(unused_imports)] // re-exported for callers naming the field types directly
pub use definitions::{ActionDef, Meta, Pin, Resolved, ResolvedStep, StepRef, Workflow};
pub(crate) use definitions::{parse_action, parse_workflow, text_writes_hidden_tests};
pub use judgment::{OUTCOME_QUESTION, Question};
pub use library::{FRAGMENTS_DIR, Include, UNTRUSTED_DATA, text_hash};
use library::{fragment_problems, load_prompt_file};
pub use lint::{LintProblem, lint};
#[allow(unused_imports)] // re-exported for callers naming the return type directly
pub use project::{
    JobSource, fixtures_at, load_all_at, resolve_job_at, resolve_job_for_project,
    resolve_job_in_repo,
};
pub use resolve::{Problem, RunStep, check, resolve, resolve_job};
use resolve::{check_flow, job_steps, splice};
pub use trigger::{
    EffectKind, Limits, OnFailure, Output, Trigger, TriggerOn, default_input_bytes, parse_duration,
};
use trigger::{TriggerRaw, build_trigger};
use validate::error_line;
#[allow(unused_imports)] // re-exported for callers naming the return type directly
pub use validate::{ValidateProblem, ValidateReport, resolve_jobs_in_tree, validate_repo};

/// Names the engine inserts itself; a user operation may not shadow them.
pub const KERNEL_OPS: &[&str] = &["verify", "push", "integrate", "land", "clone"];
