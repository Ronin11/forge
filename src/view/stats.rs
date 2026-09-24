use crate::ctx::Forge;
use crate::store::{JournalStat, StepStat, Task, WorkflowStat};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

/// One row of `StatsDoc.workflows`: outcomes for one workflow (name +
/// definition hash), as both `forge stats` and `forge stats --json` show
/// it. `legacy` carries the header-named keys (`WF`, `HASH`, `TASKS`,
/// `OK`, `FAIL`, `BLK`, `UNV`, `ATT`, `COST`, `$/OK`, `LANDED`,
/// `$/LANDED`) the JSON form emitted before the named fields below
/// existed; it is flattened onto this row so both sets of keys are
/// present on `--json` output. Deprecated: kept for one release only,
/// read the named fields instead.
#[derive(Serialize)]
pub struct StatsWorkflowRow {
    /// Workflow name.
    pub workflow: String,
    /// Hash of the workflow definition this row's tasks ran with.
    pub hash: String,
    /// Number of tasks run under this workflow + hash.
    pub pieces: i64,
    /// Tasks that finished in state `succeeded`.
    pub succeeded: i64,
    /// Tasks that finished in state `failed`.
    pub failed: i64,
    /// Tasks that finished in state `blocked`.
    pub blocked: i64,
    /// Tasks that finished in state `unverified`.
    pub unverified: i64,
    /// Attempts run across all of this workflow's tasks.
    pub attempts: i64,
    /// Cost, in USD, of every attempt across this workflow's tasks.
    pub mean_cost_usd: f64,
    /// `mean_cost_usd` divided by `succeeded`; `None` when nothing succeeded.
    pub cost_per_success_usd: Option<f64>,
    /// Tasks that landed on their base branch.
    pub landed: i64,
    /// `mean_cost_usd` divided by `landed`; `None` when nothing landed.
    pub cost_per_landed_usd: Option<f64>,
    /// Landed tasks whose `landed_sha` became a later task's `base_sha`,
    /// where that later task's first `code` attempt carries a failing L1
    /// verdict row on an unmodified base.
    pub broke_base: i64,
    /// `broke_base` divided by `landed`; `None` when nothing landed.
    pub broke_base_share: Option<f64>,
    /// Landed tasks named by a later task's `repairs` reference.
    pub repaired: i64,
    /// `repaired` divided by `landed`; `None` when nothing landed.
    pub repaired_share: Option<f64>,
    /// Delayed cost, line-overlap attribution: for each later landing on
    /// the same repository within 30 days, the fraction of its cost equal
    /// to the lines it removed or rewrote that a landed task's own
    /// landing added, divided by all the lines it removed or rewrote —
    /// summed across this workflow's landed tasks (see docs/LATER.md, the
    /// delayed-cost follow-up to "Defect escape"). `Store::WorkflowStat::repair_cost`.
    pub repair_cost_usd: f64,
    /// `(mean_cost_usd + repair_cost_usd) / landed`: what a piece of
    /// work in this workflow actually cost once its delayed cost is in,
    /// not only what landing it cost. `None` when nothing landed.
    pub true_cost_per_landed_usd: Option<f64>,
    /// Churn: of the lines this workflow's landed tasks added, the share
    /// a later landing on the same repository removed or rewrote within
    /// 30 days (`task_churn`, cached per task). `None` when nothing was
    /// added yet to measure.
    pub churn_share: Option<f64>,
    /// Verified rate: `succeeded / pieces` (0 when `pieces` is 0) — the
    /// same quantity `crate::profile::Profile::rate` reports, over this
    /// version's own tasks in scope rather than a lookback window.
    pub rate: f64,
    /// Wilson 95% interval on `rate` (`crate::profile::wilson`), the
    /// interval the `/stats` workflows tab draws as a bar.
    pub rate_lo: f64,
    pub rate_hi: f64,
    /// Set on a workflow's current version (`Store::workflow_versions`'s
    /// first hash) when its `rate` interval sits entirely below its
    /// previous version's — `crate::profile::regressed`'s rule, applied
    /// to this row's own counts; see `mark_workflow_regressions`. Always
    /// `false` on every other version, and on a current version with no
    /// previous one, or with fewer than `crate::profile::MIN_N` pieces
    /// on either side.
    pub regressed: bool,
    #[serde(flatten)]
    pub legacy: serde_json::Map<String, Value>,
}

impl From<&WorkflowStat> for StatsWorkflowRow {
    fn from(w: &WorkflowStat) -> Self {
        let cost_per_success_usd = (w.succeeded > 0).then(|| w.cost / w.succeeded as f64);
        let cost_per_landed_usd = (w.landed > 0).then(|| w.cost / w.landed as f64);
        let broke_base_share = (w.landed > 0).then(|| w.broke_base as f64 / w.landed as f64);
        let repaired_share = (w.landed > 0).then(|| w.repaired as f64 / w.landed as f64);
        let true_cost_per_landed_usd =
            (w.landed > 0).then(|| (w.cost + w.repair_cost) / w.landed as f64);
        let churn_share =
            (w.added_lines > 0).then(|| w.churned_lines as f64 / w.added_lines as f64);
        let rate = if w.tasks > 0 {
            w.succeeded as f64 / w.tasks as f64
        } else {
            0.0
        };
        let (rate_lo, rate_hi) = crate::profile::wilson(w.succeeded as usize, w.tasks as usize);
        let mut legacy = serde_json::Map::new();
        legacy.insert("WF".into(), Value::from(w.workflow.clone()));
        legacy.insert("HASH".into(), Value::from(w.hash.clone()));
        legacy.insert("TASKS".into(), Value::from(w.tasks));
        legacy.insert("OK".into(), Value::from(w.succeeded));
        legacy.insert("FAIL".into(), Value::from(w.failed));
        legacy.insert("BLK".into(), Value::from(w.blocked));
        legacy.insert("UNV".into(), Value::from(w.unverified));
        legacy.insert("ATT".into(), Value::from(w.attempts));
        legacy.insert("COST".into(), Value::from(w.cost));
        legacy.insert("$/OK".into(), serde_json::json!(cost_per_success_usd));
        legacy.insert("LANDED".into(), Value::from(w.landed));
        legacy.insert("$/LANDED".into(), serde_json::json!(cost_per_landed_usd));
        StatsWorkflowRow {
            workflow: w.workflow.clone(),
            hash: w.hash.clone(),
            pieces: w.tasks,
            succeeded: w.succeeded,
            failed: w.failed,
            blocked: w.blocked,
            unverified: w.unverified,
            attempts: w.attempts,
            mean_cost_usd: w.cost,
            cost_per_success_usd,
            landed: w.landed,
            cost_per_landed_usd,
            broke_base: w.broke_base,
            broke_base_share,
            repaired: w.repaired,
            repaired_share,
            repair_cost_usd: w.repair_cost,
            true_cost_per_landed_usd,
            churn_share,
            rate,
            rate_lo,
            rate_hi,
            regressed: false,
            legacy,
        }
    }
}

/// One row of `StatsDoc.steps`: outcomes for one workflow step, as both
/// `forge stats` and `forge stats --json` show it. `legacy` carries the
/// header-named keys (`WF`, `STEP`, `ATT`, `OK`, `AGENTF`, `CHECKF`,
/// `ASK`, `TURNS`, `EDIT@`, `SECS`, `COST`, `TOKENS`) the JSON form
/// emitted before the named fields below existed; it is flattened onto
/// this row so both sets of keys are present on `--json` output.
/// Deprecated: kept for one release only, read the named fields instead.
#[derive(Serialize)]
pub struct StatsStepRow {
    /// Workflow name.
    pub workflow: String,
    /// Step name within the workflow.
    pub step: String,
    /// Attempts run at this step.
    pub attempts: i64,
    /// Attempts that finished in state `succeeded`.
    pub succeeded: i64,
    /// Attempts that finished in state `agent_failed`.
    pub agent_failed: i64,
    /// Attempts that finished in state `checks_failed`.
    pub checks_failed: i64,
    /// Attempts that finished in state `needs_input`.
    pub needs_input: i64,
    /// Mean number of agent turns per attempt.
    pub mean_turns: f64,
    /// Mean tool calls before the first edit, over attempts that edited;
    /// `None` when none did.
    pub mean_first_edit: Option<f64>,
    /// Mean wall-clock seconds per attempt.
    pub mean_secs: f64,
    /// Cost, in USD, of every attempt at this step.
    pub cost_usd: f64,
    /// Mean input tokens, over attempts that reported usage; `None` when
    /// none did.
    pub mean_input_tokens: Option<f64>,
    #[serde(flatten)]
    pub legacy: serde_json::Map<String, Value>,
}

impl From<&StepStat> for StatsStepRow {
    fn from(st: &StepStat) -> Self {
        let mean_secs = st.mean_ms / 1000.0;
        let mut legacy = serde_json::Map::new();
        legacy.insert("WF".into(), Value::from(st.workflow.clone()));
        legacy.insert("STEP".into(), Value::from(st.step.clone()));
        legacy.insert("ATT".into(), Value::from(st.attempts));
        legacy.insert("OK".into(), Value::from(st.succeeded));
        legacy.insert("AGENTF".into(), Value::from(st.agent_failed));
        legacy.insert("CHECKF".into(), Value::from(st.checks_failed));
        legacy.insert("ASK".into(), Value::from(st.needs_input));
        legacy.insert("TURNS".into(), Value::from(st.mean_turns));
        legacy.insert("EDIT@".into(), serde_json::json!(st.mean_first_edit));
        legacy.insert("SECS".into(), Value::from(mean_secs));
        legacy.insert("COST".into(), Value::from(st.cost));
        legacy.insert("TOKENS".into(), serde_json::json!(st.mean_input_tokens));
        StatsStepRow {
            workflow: st.workflow.clone(),
            step: st.step.clone(),
            attempts: st.attempts,
            succeeded: st.succeeded,
            agent_failed: st.agent_failed,
            checks_failed: st.checks_failed,
            needs_input: st.needs_input,
            mean_turns: st.mean_turns,
            mean_first_edit: st.mean_first_edit,
            mean_secs,
            cost_usd: st.cost,
            mean_input_tokens: st.mean_input_tokens,
            legacy,
        }
    }
}

/// One side of `StatsDoc.journal` / `StatsDoc.no_journal`: code attempts
/// after the first (`attempt_no > 1`), for attempts that either were or
/// were not handed a journal. See docs/LATER.md, "The journal measurement
/// was ill-posed three times".
#[derive(Serialize, Default)]
pub struct StatsJournalRow {
    /// Code retries in this arm.
    pub attempts: i64,
    /// Of those, how many finished in state `succeeded`.
    pub succeeded: i64,
    /// `succeeded` divided by `attempts`; `None` when there are none.
    pub succeeded_share: Option<f64>,
    /// Mean agent turns per attempt.
    pub mean_turns: f64,
    /// Mean tool calls before the first edit, over attempts that edited;
    /// `None` when none did.
    pub mean_first_edit: Option<f64>,
    /// Mean cost in USD per attempt.
    pub mean_cost_usd: f64,
}

impl From<&JournalStat> for StatsJournalRow {
    fn from(j: &JournalStat) -> Self {
        StatsJournalRow {
            attempts: j.attempts,
            succeeded: j.succeeded,
            succeeded_share: (j.attempts > 0).then(|| j.succeeded as f64 / j.attempts as f64),
            mean_turns: j.mean_turns,
            mean_first_edit: j.mean_first_edit,
            mean_cost_usd: j.mean_cost_usd,
        }
    }
}

/// One row of `StatsDoc.projects`: tasks, landed count, cost and defect
/// escape for one project, shown when `forge stats` is not itself scoped
/// to a project or initiative.
#[derive(Serialize)]
pub struct StatsProjectRow {
    pub project: String,
    pub tasks: i64,
    pub landed: i64,
    pub cost_usd: f64,
    /// See `StatsWorkflowRow::broke_base`.
    pub broke_base: i64,
    /// `broke_base` divided by `landed`; `None` when nothing landed.
    pub broke_base_share: Option<f64>,
}

impl From<&crate::store::ProjectStat> for StatsProjectRow {
    fn from(p: &crate::store::ProjectStat) -> Self {
        StatsProjectRow {
            project: p.project.clone(),
            tasks: p.tasks,
            landed: p.landed,
            cost_usd: p.cost,
            broke_base: p.broke_base,
            broke_base_share: (p.landed > 0).then(|| p.broke_base as f64 / p.landed as f64),
        }
    }
}

/// One row of `StatsDoc.human_attention`: human attention for one
/// workflow version — what a person had to do for its landed work, since
/// minutes cannot be measured (docs/LATER.md, "Two metrics the record can
/// compute and does not"). Four events, summed as `events` and divided by
/// `landed` as `events_per_landed`.
#[derive(Serialize)]
pub struct HumanAttentionRow {
    pub workflow: String,
    pub hash: String,
    pub landed: i64,
    /// Decisions on this workflow's tasks with `answered_by` other than
    /// `"supervisor"` (an operator, or a channel contact).
    pub operator_answers: i64,
    /// This workflow's tasks landed by a human's `forge land`.
    pub hand_landed: i64,
    /// This workflow's tasks left `withdrawn`.
    pub withdrawals: i64,
    /// Commits not authored as Forge, on the base branch, between this
    /// workflow's landings and the ones before them.
    pub hand_commits: i64,
    /// The four counts above, summed.
    pub events: i64,
    /// `events` divided by `landed`; `None` when nothing landed.
    pub events_per_landed: Option<f64>,
}

impl From<&crate::store::HumanAttentionStat> for HumanAttentionRow {
    fn from(h: &crate::store::HumanAttentionStat) -> Self {
        let events = h.operator_answers + h.hand_landed + h.withdrawals + h.hand_commits;
        HumanAttentionRow {
            workflow: h.workflow.clone(),
            hash: h.hash.clone(),
            landed: h.landed,
            operator_answers: h.operator_answers,
            hand_landed: h.hand_landed,
            withdrawals: h.withdrawals,
            hand_commits: h.hand_commits,
            events,
            events_per_landed: (h.landed > 0).then(|| events as f64 / h.landed as f64),
        }
    }
}

/// One row of `StatsDoc.human_attention_projects`: the same four signals
/// as `HumanAttentionRow`, over one project's tasks instead of one
/// workflow version's; shown under the same scoping rule as `projects`.
#[derive(Serialize)]
pub struct HumanAttentionProjectRow {
    pub project: String,
    pub landed: i64,
    pub operator_answers: i64,
    pub hand_landed: i64,
    pub withdrawals: i64,
    pub hand_commits: i64,
    pub events: i64,
    pub events_per_landed: Option<f64>,
}

impl From<&crate::store::HumanAttentionProjectStat> for HumanAttentionProjectRow {
    fn from(h: &crate::store::HumanAttentionProjectStat) -> Self {
        let events = h.operator_answers + h.hand_landed + h.withdrawals + h.hand_commits;
        HumanAttentionProjectRow {
            project: h.project.clone(),
            landed: h.landed,
            operator_answers: h.operator_answers,
            hand_landed: h.hand_landed,
            withdrawals: h.withdrawals,
            hand_commits: h.hand_commits,
            events,
            events_per_landed: (h.landed > 0).then(|| events as f64 / h.landed as f64),
        }
    }
}

/// One row of `StatsDoc.time_to_live`: how long a request took to go live,
/// for one workflow version's landed tasks (docs/LATER.md, "Two metrics
/// the record can compute and does not"). Per task, that is
/// `landed_at - created_at`, or, when a deploy is tied to the task, that
/// deploy's `finished_at - created_at` instead (see `Store::task_ttls`).
#[derive(Serialize)]
pub struct TimeToLiveRow {
    pub workflow: String,
    pub hash: String,
    /// How many landed tasks this rests on.
    pub n: i64,
    /// `None` when `n` is 0.
    pub median_secs: Option<f64>,
    /// `None` when `n` is 0.
    pub p90_secs: Option<f64>,
}

/// One row of `StatsDoc.time_to_live_projects`: the same measure as
/// `TimeToLiveRow`, over one project's landed tasks instead of one
/// workflow version's; shown under the same scoping rule as `projects`.
#[derive(Serialize)]
pub struct TimeToLiveProjectRow {
    pub project: String,
    pub n: i64,
    pub median_secs: Option<f64>,
    pub p90_secs: Option<f64>,
}

/// The median and 90th percentile of `secs`, nearest-rank on the sorted
/// list (so the percentile is always one of the actual values, never an
/// interpolation) — `(None, None)` when `secs` is empty.
fn median_p90(secs: &mut [i64]) -> (Option<f64>, Option<f64>) {
    if secs.is_empty() {
        return (None, None);
    }
    secs.sort_unstable();
    let at = |p: f64| -> f64 {
        let rank = ((p * secs.len() as f64).ceil() as usize)
            .max(1)
            .min(secs.len());
        secs[rank - 1] as f64
    };
    (Some(at(0.5)), Some(at(0.9)))
}

/// One row of `StatsDoc.jobs`: one project's jobs in the last rolling 24h,
/// by outcome — counted separately from `StatsProjectRow`'s task rollup
/// (docs/JOBS.md step 1d).
#[derive(Serialize)]
pub struct StatsJobsRow {
    pub project: String,
    pub today: i64,
    pub ok: i64,
    pub failed: i64,
    pub needs_human: i64,
    pub skipped: i64,
}

impl From<&crate::store::JobStat> for StatsJobsRow {
    fn from(j: &crate::store::JobStat) -> Self {
        StatsJobsRow {
            project: j.project.clone(),
            today: j.today,
            ok: j.ok,
            failed: j.failed,
            needs_human: j.needs_human,
            skipped: j.skipped,
        }
    }
}

/// One row of `StatsDoc.by_role`: attempts, outcomes, cost and wall time
/// for one (role, provider, model, kind) combination, role being the
/// attempt's step or the job step's action, and kind (`"attempt"` or
/// `"job_step"`) distinguishing a build task's attempt from a job's
/// directive step, as `forge stats --by-role` shows it.
#[derive(Serialize)]
pub struct StatsRoleRow {
    pub role: String,
    pub provider: String,
    pub model: String,
    pub kind: String,
    pub attempts: i64,
    pub succeeded: i64,
    /// `succeeded` divided by `attempts`; `None` when there are none.
    pub succeeded_share: Option<f64>,
    pub mean_turns: f64,
    pub mean_cost_usd: f64,
    pub mean_secs: f64,
    /// Landed tasks with an attempt in this group; `None` outside the
    /// `code` role. See `StatsWorkflowRow::broke_base`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landed: Option<i64>,
    /// Of `landed`, how many broke a later task's base; `None` outside the
    /// `code` role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broke_base: Option<i64>,
    /// `broke_base` divided by `landed`; `None` when `landed` is `None` or 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broke_base_share: Option<f64>,
    /// See `StatsWorkflowRow::repair_cost_usd`, summed over this
    /// group's own landed tasks; `None` outside the `code` role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_cost_usd: Option<f64>,
    /// See `StatsWorkflowRow::true_cost_per_landed_usd`; `None` outside
    /// the `code` role or when `landed` is `None` or 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_cost_per_landed_usd: Option<f64>,
    /// See `StatsWorkflowRow::churn_share`; `None` outside the `code`
    /// role or when nothing was added yet to measure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub churn_share: Option<f64>,
}

impl From<&crate::store::RoleStat> for StatsRoleRow {
    fn from(r: &crate::store::RoleStat) -> Self {
        let broke_base_share = match (r.landed, r.broke_base) {
            (Some(landed), Some(broke)) if landed > 0 => Some(broke as f64 / landed as f64),
            _ => None,
        };
        let true_cost_per_landed_usd = match (r.landed, r.repair_cost) {
            (Some(landed), Some(repair_cost)) if landed > 0 => {
                Some((r.mean_cost_usd * r.attempts as f64 + repair_cost) / landed as f64)
            }
            _ => None,
        };
        let churn_share = match (r.added_lines, r.churned_lines) {
            (Some(added), Some(churned)) if added > 0 => Some(churned as f64 / added as f64),
            _ => None,
        };
        StatsRoleRow {
            role: r.role.clone(),
            provider: r.provider.clone(),
            model: r.model.clone(),
            kind: r.kind.clone(),
            attempts: r.attempts,
            succeeded: r.succeeded,
            succeeded_share: (r.attempts > 0).then(|| r.succeeded as f64 / r.attempts as f64),
            mean_turns: r.mean_turns,
            mean_cost_usd: r.mean_cost_usd,
            mean_secs: r.mean_ms / 1000.0,
            landed: r.landed,
            broke_base: r.broke_base,
            broke_base_share,
            repair_cost_usd: r.repair_cost,
            true_cost_per_landed_usd,
            churn_share,
        }
    }
}

/// One row of `StatsDoc.daily`: one UTC date's landings and spend, the
/// `/stats` page's 30-day chart source. See `Store::DailyStat` and
/// `Store::daily_stats`.
#[derive(Serialize)]
pub struct StatsDailyRow {
    pub date: String,
    pub landed: i64,
    pub cost_usd: f64,
}

impl From<&crate::store::DailyStat> for StatsDailyRow {
    fn from(d: &crate::store::DailyStat) -> Self {
        StatsDailyRow {
            date: d.date.clone(),
            landed: d.landed,
            cost_usd: d.cost_usd,
        }
    }
}

/// Everything `forge stats` shows: outcomes per workflow, outcomes per
/// step, the journal control arm's retrospective split (with `--journal`),
/// and (with `--tools`) tool usage per step. `forge stats --json`
/// serializes this directly; `forge stats` renders the same tables as
/// text from the same rows.
#[derive(Serialize)]
pub struct StatsDoc {
    pub workflows: Vec<StatsWorkflowRow>,
    pub steps: Vec<StatsStepRow>,
    /// Code retries that were handed a journal.
    pub journal: StatsJournalRow,
    /// Code retries that were not.
    pub no_journal: StatsJournalRow,
    /// Tasks, landed count, cost and defect escape per project; only
    /// filled when `stats_doc` was called with no project/initiative
    /// scope of its own (see docs/PROJECTS.md, "The record, scoped").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<StatsProjectRow>,
    /// Jobs started in the last rolling 24h, per project, by outcome; same
    /// scoping rule as `projects` (docs/JOBS.md step 1d).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<StatsJobsRow>,
    /// Attempts, outcomes, cost and wall time per (role, provider, model);
    /// see `forge stats --by-role`.
    pub by_role: Vec<StatsRoleRow>,
    /// Spearman's rank correlation between the assess directive's score
    /// and each delayed-cost measure, over this scope's landed tasks
    /// that carry both; see `forge stats --quality` and
    /// `quality_correlation`.
    pub assessment_correlation: Vec<CorrelationRow>,
    /// Human attention per workflow version: what a person had to do for
    /// its landed work (see `HumanAttentionRow`, `forge stats --quality`).
    pub human_attention: Vec<HumanAttentionRow>,
    /// Human attention per project; same scoping rule as `projects`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub human_attention_projects: Vec<HumanAttentionProjectRow>,
    /// Time to live per workflow version: how long a request took to go
    /// live (see `TimeToLiveRow`, `forge stats --quality`).
    pub time_to_live: Vec<TimeToLiveRow>,
    /// Time to live per project; same scoping rule as `projects`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub time_to_live_projects: Vec<TimeToLiveProjectRow>,
    /// One row per factor level, over this scope's landed and failed
    /// tasks in the window `--days` names (every one of them, absent a
    /// window); see `forge stats --factors` and `StatsFactorRow`.
    pub factors: Vec<StatsFactorRow>,
    /// The last 30 UTC days' landings and spend, oldest first — the
    /// `/stats` page's chart source (see `StatsDailyRow`).
    pub daily: Vec<StatsDailyRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
}

/// One row of `StatsDoc.factors`: one level of one factor in `forge
/// stats --factors` (docs/ECONOMIST.md, piece 3) — `factor` is
/// `"provider:<role>"` (the provider a role ran under, one factor per
/// role that ran in scope), `"workflow"`, or `"size"` (the task-shape
/// bin from `crate::store::size_class`); `level` is that factor's value
/// (a provider name, a workflow name, or `"small"`/`"medium"`/`"large"`).
///
/// `tasks`, `landed`, `rate` (with its Wilson 95% interval, `rate_lo`/
/// `rate_hi`) and `mean_true_cost_usd` (null when nothing in this level
/// landed) describe this level's own tasks. `effect` and `effect_se`
/// come from one joint least-squares fit of `ln(true cost)` on every
/// factor and level in scope at once (main effects only, no
/// interactions): the change in log cost this level carries against its
/// factor's reference level (`is_reference`), with a standard error.
/// Both are null for the reference level itself, and for any level
/// whose factor the fit could not identify (stuck at one level among
/// the tasks that landed, too little data for the columns in play, or
/// perfectly confounded with another factor) — see
/// `crate::store::Store::factor_stats`.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct StatsFactorRow {
    pub factor: String,
    pub level: String,
    pub tasks: i64,
    pub landed: i64,
    pub rate: f64,
    pub rate_lo: f64,
    pub rate_hi: f64,
    pub mean_true_cost_usd: Option<f64>,
    pub is_reference: bool,
    pub effect: Option<f64>,
    pub effect_se: Option<f64>,
    /// Exploration over the level's tasks' code attempts: the mean tool
    /// call at which the first edit came, and the mean tool calls per
    /// turn; `null` when no attempt recorded them.
    pub mean_first_edit_call: Option<f64>,
    pub mean_calls_per_turn: Option<f64>,
    pub mean_grep_then_ranged_read_chains: Option<f64>,
    pub mean_unedited_read_chars: Option<f64>,
    pub mean_turns_before_first_edit: Option<f64>,
    pub mean_outline_calls: Option<f64>,
    pub mean_def_calls: Option<f64>,
}

impl From<&crate::store::FactorLevelStat> for StatsFactorRow {
    fn from(f: &crate::store::FactorLevelStat) -> Self {
        StatsFactorRow {
            factor: f.factor.clone(),
            level: f.level.clone(),
            tasks: f.tasks,
            landed: f.landed,
            rate: f.rate,
            rate_lo: f.rate_lo,
            rate_hi: f.rate_hi,
            mean_true_cost_usd: f.mean_true_cost_usd,
            is_reference: f.is_reference,
            effect: f.effect,
            effect_se: f.effect_se,
            mean_first_edit_call: f.mean_first_edit_call,
            mean_calls_per_turn: f.mean_calls_per_turn,
            mean_grep_then_ranged_read_chains: f.mean_grep_then_ranged_read_chains,
            mean_unedited_read_chars: f.mean_unedited_read_chars,
            mean_turns_before_first_edit: f.mean_turns_before_first_edit,
            mean_outline_calls: f.mean_outline_calls,
            mean_def_calls: f.mean_def_calls,
        }
    }
}

/// One row of `StatsDoc.assessment_correlation`: how well the assess
/// directive's fast proxy score tracks one delayed-cost measure that only
/// shows up once later tasks land (docs/ACTIONS.md, "Assessment";
/// docs/LATER.md's delayed-cost follow-up to "Defect escape") — Spearman's
/// rank correlation, over this scope's landed tasks carrying both an
/// assessment and the measure.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct CorrelationRow {
    /// `"churn"` (per task, the share of its added lines a later landing
    /// rewrote) or `"repair_cost"` (per task, its cached repair cost in
    /// USD).
    pub measure: String,
    /// Spearman's rho; `None` when fewer than two tasks carry both the
    /// score and this measure, or either side has no variance to rank.
    pub rho: Option<f64>,
    /// How many landed, assessed tasks this rests on.
    pub n: i64,
}

/// Spearman's rank correlation between the assess directive's score and
/// each delayed-cost measure, over `scope`'s landed tasks that carry an
/// assessment and the measure in question: `churn` (per task, its cached
/// `(churned, added)` lines as `churned / added`, omitted when `added` is
/// 0) and `repair_cost` (per task, its cached repair cost in USD; see
/// `refresh_churn` and `refresh_repair_cost`, both already run by the
/// time `stats_doc` calls this). The two measures can rest on different
/// task counts, since a task can have one cached without the other.
fn quality_correlation(
    f: &Forge,
    scope: &crate::store::StatsFilter,
) -> Result<Vec<CorrelationRow>> {
    let mut churn_pairs = Vec::new();
    let mut repair_pairs = Vec::new();
    for t in f.store.landed_tasks(scope)? {
        let Some(a) = f.store.assessment(t.id)? else {
            continue;
        };
        let score = a.score as f64;
        if let Some((added, churned, _)) = f.store.churn_cache(t.id)?
            && added > 0
        {
            churn_pairs.push((score, churned as f64 / added as f64));
        }
        if let Some((repair_cost, _)) = f.store.repair_cost_cache(t.id)? {
            repair_pairs.push((score, repair_cost));
        }
    }
    Ok(vec![
        CorrelationRow {
            measure: "churn".into(),
            n: churn_pairs.len() as i64,
            rho: spearman(&churn_pairs),
        },
        CorrelationRow {
            measure: "repair_cost".into(),
            n: repair_pairs.len() as i64,
            rho: spearman(&repair_pairs),
        },
    ])
}

/// Spearman's rank correlation over `pairs`: Pearson's r computed on each
/// side's ranks (ties broken by averaging), which is Spearman's rho with
/// the standard tie correction. `None` when fewer than two pairs, or
/// either side is constant (an undefined correlation).
fn spearman(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 2 {
        return None;
    }
    let xs: Vec<f64> = pairs.iter().map(|p| p.0).collect();
    let ys: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    pearson(&rank(&xs), &rank(&ys))
}

/// 1-based ranks of `v`, tied values given their shared average rank.
fn rank(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].partial_cmp(&v[b]).unwrap());
    let mut ranks = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            ranks[k] = avg_rank;
        }
        i = j + 1;
    }
    ranks
}

/// Pearson's r between `a` and `b`, `None` when either has zero variance.
fn pearson(a: &[f64], b: &[f64]) -> Option<f64> {
    let n = a.len() as f64;
    let mean_a = a.iter().sum::<f64>() / n;
    let mean_b = b.iter().sum::<f64>() / n;
    let (mut cov, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    for i in 0..a.len() {
        let da = a[i] - mean_a;
        let db = b[i] - mean_b;
        cov += da * db;
        var_a += da * da;
        var_b += db * db;
    }
    if var_a == 0.0 || var_b == 0.0 {
        return None;
    }
    Some(cov / (var_a.sqrt() * var_b.sqrt()))
}

/// Refresh `task_churn` for every landed task (scope does not narrow this:
/// `by_role` and an unscoped `forge stats` both read the whole table, so a
/// scoped call would leave the rest stale) whose 30-day window has not yet
/// closed as of its last computation, or that has never been computed:
/// the only part of `forge stats` that reads git rather than the store,
/// since churn is a diff over the repository's own history (docs/LATER.md,
/// the delayed-cost follow-up to "Defect escape"). Once a task's window
/// has closed, its cache is never touched again.
async fn refresh_churn(f: &Forge) -> Result<()> {
    let now = crate::unix_now();
    for t in f
        .store
        .landed_tasks(&crate::store::StatsFilter::default())?
    {
        let Some(finished_at) = t.finished_at else {
            continue;
        };
        let window_closes = finished_at + crate::store::THIRTY_DAYS_SECS;
        let stale = match f.store.churn_cache(t.id)? {
            Some((_, _, computed_at)) => computed_at < window_closes,
            None => true,
        };
        if !stale {
            continue;
        }
        let (added, churned) = compute_churn(f, &t, finished_at).await?;
        f.store.set_churn_cache(t.id, added, churned, now)?;
    }
    Ok(())
}

/// One landed task's churn: lines its landing added (`base_sha` to
/// `landed_sha`), and of those, how many exact `(path, content)` pairs a
/// later landing on the same repository removed or rewrote within 30 days
/// of this one landing.
async fn compute_churn(f: &Forge, t: &Task, finished_at: i64) -> Result<(i64, i64)> {
    let repo = std::path::Path::new(&t.repo);
    let (added, _) = crate::git::diff_lines(repo, &t.base_sha, &t.landed_sha).await?;
    if added.is_empty() {
        return Ok((0, 0));
    }
    let window_end = finished_at + crate::store::THIRTY_DAYS_SECS;
    let later = f
        .store
        .later_landings(&t.repo, t.id, finished_at, window_end)?;
    let mut removed_set: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for u in later {
        let (_, removed) = crate::git::diff_lines(repo, &u.base_sha, &u.landed_sha).await?;
        removed_set.extend(removed);
    }
    let churned = added
        .iter()
        .filter(|line| removed_set.contains(*line))
        .count() as i64;
    Ok((added.len() as i64, churned))
}

/// Refresh `task_repair_cost` for every landed task, on the same
/// stale-until-the-window-closes schedule as `refresh_churn`: the REPAIRCOST
/// column, replacing task 357's path-overlap follow-on cost (which charged
/// a landed task the whole cost of any later task that changed any path it
/// changed) with a line-level attribution — a later landing only charges an
/// earlier one for the fraction of its own cost spent rewriting that
/// earlier task's actual lines.
async fn refresh_repair_cost(f: &Forge) -> Result<()> {
    let now = crate::unix_now();
    for t in f
        .store
        .landed_tasks(&crate::store::StatsFilter::default())?
    {
        let Some(finished_at) = t.finished_at else {
            continue;
        };
        let window_closes = finished_at + crate::store::THIRTY_DAYS_SECS;
        let stale = match f.store.repair_cost_cache(t.id)? {
            Some((_, computed_at)) => computed_at < window_closes,
            None => true,
        };
        if !stale {
            continue;
        }
        let repair_cost = compute_repair_cost(f, &t, finished_at).await?;
        f.store.set_repair_cost_cache(t.id, repair_cost, now)?;
    }
    Ok(())
}

/// Multiset intersection: how many of `b`'s elements (with multiplicity)
/// also appear in `a`, each element of `a` usable at most once. Order-
/// independent (`min(count_a[x], count_b[x])` summed over every `x`).
fn multiset_overlap(a: &[(String, String)], b: &[(String, String)]) -> i64 {
    let mut counts: std::collections::HashMap<&(String, String), i64> =
        std::collections::HashMap::new();
    for x in a {
        *counts.entry(x).or_insert(0) += 1;
    }
    let mut overlap = 0i64;
    for y in b {
        if let Some(c) = counts.get_mut(y)
            && *c > 0
        {
            *c -= 1;
            overlap += 1;
        }
    }
    overlap
}

/// One landed task T's repair cost: for each later landing L on the same
/// repository within 30 days, the number of lines T's landing added (`T.
/// base_sha` to `T.landed_sha`) that L's landing removed or rewrote (`L.
/// base_sha` to `L.landed_sha`, the removed side — the same line-level
/// diff `compute_churn` reads), divided by all the lines L's landing
/// removed or rewrote, times L's total cost; summed over every L. A later
/// landing that rewrote none of T's lines contributes nothing. The
/// per-(T, L) git computation is cached in `line_overlap_cache`, keyed by
/// both landed commits, so it only ever runs once.
async fn compute_repair_cost(f: &Forge, t: &Task, finished_at: i64) -> Result<f64> {
    let repo = std::path::Path::new(&t.repo);
    let (t_added, _) = crate::git::diff_lines(repo, &t.base_sha, &t.landed_sha).await?;
    if t_added.is_empty() {
        return Ok(0.0);
    }
    let window_end = finished_at + crate::store::THIRTY_DAYS_SECS;
    let later = f
        .store
        .later_landings(&t.repo, t.id, finished_at, window_end)?;
    let mut total = 0.0;
    for l in later {
        let (overlap, removed_lines) =
            match f.store.line_overlap_cache(&t.landed_sha, &l.landed_sha)? {
                Some(pair) => pair,
                None => {
                    let (_, l_removed) =
                        crate::git::diff_lines(repo, &l.base_sha, &l.landed_sha).await?;
                    let overlap = multiset_overlap(&t_added, &l_removed);
                    let removed_lines = l_removed.len() as i64;
                    f.store.set_line_overlap_cache(
                        &t.landed_sha,
                        &l.landed_sha,
                        overlap,
                        removed_lines,
                    )?;
                    (overlap, removed_lines)
                }
            };
        if removed_lines == 0 {
            continue;
        }
        let fraction = overlap as f64 / removed_lines as f64;
        total += fraction * f.store.task_cost(l.id)?;
    }
    Ok(total)
}

/// Refresh `task_hand_commits` for every landed task that has never had it
/// computed: unlike `refresh_churn`/`refresh_repair_cost`, there is no
/// stale-until-a-window-closes schedule, since neither endpoint of the
/// range this counts (the previous landing's `landed_sha`, this task's own
/// `base_sha`) ever changes once the task has landed.
async fn refresh_hand_commits(f: &Forge) -> Result<()> {
    let now = crate::unix_now();
    for t in f
        .store
        .landed_tasks(&crate::store::StatsFilter::default())?
    {
        if f.store.hand_commits_cache(t.id)?.is_some() {
            continue;
        }
        let hand_commits = compute_hand_commits(f, &t).await?;
        f.store.set_hand_commits_cache(t.id, hand_commits, now)?;
    }
    Ok(())
}

/// One landed task's hand commits: commits not authored as Forge, on the
/// base branch, between the previous landing on the same repository
/// (`landed_sha`) and this task's own `base_sha` — the human attention a
/// person spent committing straight to the base while Forge was not
/// looking (docs/LATER.md, "Two metrics the record can compute and does
/// not"). Zero for the first landing a repository ever gets, since there
/// is no earlier landing to bound the range against.
async fn compute_hand_commits(f: &Forge, t: &Task) -> Result<i64> {
    let Some(prev) = f.store.previous_landing(&t.repo, t.id)? else {
        return Ok(0);
    };
    let repo = std::path::Path::new(&t.repo);
    crate::git::hand_commit_count(repo, &prev.landed_sha, &t.base_sha).await
}

/// `ttls` reduced to one `TimeToLiveRow` per workflow version present,
/// each its own median and 90th percentile.
fn time_to_live_rows(ttls: &[crate::store::TaskTtl]) -> Vec<TimeToLiveRow> {
    let mut groups: std::collections::BTreeMap<(String, String), Vec<i64>> =
        std::collections::BTreeMap::new();
    for t in ttls {
        groups
            .entry((t.workflow.clone(), t.hash.clone()))
            .or_default()
            .push(t.secs);
    }
    groups
        .into_iter()
        .map(|((workflow, hash), mut secs)| {
            let (median_secs, p90_secs) = median_p90(&mut secs);
            TimeToLiveRow {
                workflow,
                hash,
                n: secs.len() as i64,
                median_secs,
                p90_secs,
            }
        })
        .collect()
}

/// `ttls` reduced to one `TimeToLiveProjectRow` per project present, each
/// its own median and 90th percentile; a task with no project is left out,
/// same as `Store::project_stats`.
fn time_to_live_project_rows(ttls: &[crate::store::TaskTtl]) -> Vec<TimeToLiveProjectRow> {
    let mut groups: std::collections::BTreeMap<String, Vec<i64>> =
        std::collections::BTreeMap::new();
    for t in ttls {
        let Some(project) = &t.project else {
            continue;
        };
        groups.entry(project.clone()).or_default().push(t.secs);
    }
    groups
        .into_iter()
        .map(|(project, mut secs)| {
            let (median_secs, p90_secs) = median_p90(&mut secs);
            TimeToLiveProjectRow {
                project,
                n: secs.len() as i64,
                median_secs,
                p90_secs,
            }
        })
        .collect()
}

/// `days`, when given, is `forge stats --factors --days N`'s window: only
/// `factors` reads it, narrowed to tasks that finished in the last `days`
/// days; every other section reads this scope's whole history, as before.
pub async fn stats_doc(
    f: &Forge,
    scope: &crate::store::StatsFilter,
    days: Option<i64>,
) -> Result<StatsDoc> {
    refresh_churn(f).await?;
    refresh_hand_commits(f).await?;
    refresh_repair_cost(f).await?;
    let journal_stats = f.store.journal_control_stats()?;
    let journal = journal_stats
        .iter()
        .find(|j| j.has_journal)
        .map(Into::into)
        .unwrap_or_default();
    let no_journal = journal_stats
        .iter()
        .find(|j| !j.has_journal)
        .map(Into::into)
        .unwrap_or_default();
    let (projects, jobs) = if scope.project.is_none() && scope.initiative.is_none() {
        (
            f.store.project_stats()?.iter().map(Into::into).collect(),
            f.store
                .job_stats(crate::unix_now() - 86_400)?
                .iter()
                .map(Into::into)
                .collect(),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let human_attention_projects = if scope.project.is_none() && scope.initiative.is_none() {
        f.store
            .human_attention_project_stats()?
            .iter()
            .map(Into::into)
            .collect()
    } else {
        Vec::new()
    };
    let ttls = f.store.task_ttls(scope)?;
    let time_to_live_projects = if scope.project.is_none() && scope.initiative.is_none() {
        time_to_live_project_rows(&ttls)
    } else {
        Vec::new()
    };
    let mut workflows: Vec<StatsWorkflowRow> = f
        .store
        .workflow_stats(scope)?
        .iter()
        .map(Into::into)
        .collect();
    mark_workflow_regressions(f, &mut workflows)?;
    Ok(StatsDoc {
        workflows,
        steps: f.store.step_stats(scope)?.iter().map(Into::into).collect(),
        journal,
        no_journal,
        projects,
        jobs,
        by_role: f.store.role_stats()?.iter().map(Into::into).collect(),
        assessment_correlation: quality_correlation(f, scope)?,
        human_attention: f
            .store
            .human_attention_stats(scope)?
            .iter()
            .map(Into::into)
            .collect(),
        human_attention_projects,
        time_to_live: time_to_live_rows(&ttls),
        time_to_live_projects,
        factors: f
            .store
            .factor_stats(scope, days.map(|d| crate::unix_now() - d * 86_400))?
            .iter()
            .map(Into::into)
            .collect(),
        daily: f.store.daily_stats(scope)?.iter().map(Into::into).collect(),
        tools: None,
    })
}

/// Marks each `StatsWorkflowRow` whose hash is its workflow's current
/// version (`Store::workflow_versions`'s first entry) as `regressed` when
/// its own verified-rate interval sits entirely below the immediately
/// previous version's — `crate::profile::regressed`'s separated-intervals
/// rule, `crate::profile::MIN_N` gating both sides, applied to this row's
/// own in-scope counts rather than `crate::profile::measure`'s separate
/// lookback window, so the `/stats` workflows tab's regression mark
/// matches the numbers it sits beside.
fn mark_workflow_regressions(f: &Forge, rows: &mut [StatsWorkflowRow]) -> Result<()> {
    let names: std::collections::BTreeSet<String> =
        rows.iter().map(|r| r.workflow.clone()).collect();
    for name in names {
        let versions = f.store.workflow_versions(&name)?;
        let (Some(current_hash), Some(previous_hash)) = (versions.first(), versions.get(1)) else {
            continue;
        };
        let current = rows
            .iter()
            .find(|r| r.workflow == name && &r.hash == current_hash)
            .map(|r| (r.pieces, r.rate_hi));
        let previous = rows
            .iter()
            .find(|r| r.workflow == name && &r.hash == previous_hash)
            .map(|r| (r.pieces, r.rate_lo));
        let Some(((current_n, current_hi), (previous_n, previous_lo))) = current.zip(previous)
        else {
            continue;
        };
        if current_n as usize >= crate::profile::MIN_N
            && previous_n as usize >= crate::profile::MIN_N
            && current_hi < previous_lo
            && let Some(r) = rows
                .iter_mut()
                .find(|r| r.workflow == name && &r.hash == current_hash)
        {
            r.regressed = true;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod stats_tests;

#[cfg(test)]
#[path = "time_to_live_tests.rs"]
mod time_to_live_tests;
