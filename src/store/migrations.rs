//! The schema migration ladder (see `store::mod`'s doc comment): forward-only,
//! never edited once shipped, only appended to. Split out of mod.rs to keep
//! it under the 1500-line bound `no_store_file_is_over_1500_lines` checks.

/// Forward-only. Index = version - 1. Never edit a shipped entry; append.
pub const MIGRATIONS: &[&str] = &[
    "
CREATE TABLE tasks (
  id INTEGER PRIMARY KEY,
  repo TEXT NOT NULL,
  task TEXT NOT NULL,
  base_branch TEXT NOT NULL,
  base_sha TEXT NOT NULL DEFAULT '',
  branch TEXT NOT NULL DEFAULT '',
  worktree TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL,
  max_turns INTEGER NOT NULL,
  max_attempts INTEGER NOT NULL,
  timeout_secs INTEGER NOT NULL,
  checks_json TEXT NOT NULL DEFAULT '[]',
  state TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  started_at INTEGER,
  finished_at INTEGER,
  pushed INTEGER NOT NULL DEFAULT 0,
  worker_pid INTEGER,
  budget_usd REAL,
  worktree_removed_at INTEGER
);
CREATE TABLE attempts (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  attempt_no INTEGER NOT NULL,
  state TEXT NOT NULL,
  reason TEXT NOT NULL DEFAULT '',
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  agent_exit INTEGER,
  timed_out INTEGER NOT NULL DEFAULT 0,
  num_turns INTEGER NOT NULL DEFAULT 0,
  tool_calls INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL,
  agent_ms INTEGER NOT NULL DEFAULT 0,
  commits INTEGER NOT NULL DEFAULT 0,
  files_changed INTEGER NOT NULL DEFAULT 0,
  dirty INTEGER NOT NULL DEFAULT 0,
  verdict_json TEXT NOT NULL DEFAULT '[]',
  result_text TEXT NOT NULL DEFAULT '',
  log_path TEXT NOT NULL DEFAULT ''
);
CREATE INDEX attempts_task ON attempts(task_id, attempt_no);
CREATE INDEX tasks_state ON tasks(state, id);
",
    "
ALTER TABLE attempts ADD COLUMN envelope_json TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN rl_five_hour REAL;
ALTER TABLE attempts ADD COLUMN rl_seven_day REAL;
ALTER TABLE attempts ADD COLUMN rl_five_hour_resets INTEGER;
ALTER TABLE attempts ADD COLUMN rl_seven_day_resets INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN allow_protected INTEGER NOT NULL DEFAULT 0;
",
    "
ALTER TABLE tasks ADD COLUMN workflow TEXT NOT NULL DEFAULT 'direct';
ALTER TABLE tasks ADD COLUMN interface TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN show_checks INTEGER NOT NULL DEFAULT 0;
ALTER TABLE attempts ADD COLUMN step TEXT NOT NULL DEFAULT 'code';
",
    "
ALTER TABLE attempts ADD COLUMN start_sha TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN workflow_hash TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN workflow_text TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN end_sha TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN inputs_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE attempts ADD COLUMN outputs_json TEXT NOT NULL DEFAULT '{}';
",
    "
ALTER TABLE tasks ADD COLUMN actions_json TEXT NOT NULL DEFAULT '';
ALTER TABLE attempts ADD COLUMN step_seq INTEGER NOT NULL DEFAULT 0;
CREATE TABLE ops (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  seq INTEGER NOT NULL,
  name TEXT NOT NULL,
  kernel INTEGER NOT NULL,
  started_at INTEGER NOT NULL,
  ms INTEGER NOT NULL DEFAULT 0,
  ok INTEGER NOT NULL DEFAULT 0,
  exit INTEGER,
  detail TEXT NOT NULL DEFAULT '',
  attempt_id INTEGER
);
CREATE INDEX ops_task ON ops(task_id, id);
",
    "
ALTER TABLE ops ADD COLUMN output TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN land INTEGER NOT NULL DEFAULT 1;
",
    "
ALTER TABLE attempts ADD COLUMN session_id TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN after_json TEXT NOT NULL DEFAULT '[]';
",
    "
ALTER TABLE tasks ADD COLUMN verify_base TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE tasks ADD COLUMN retry_of INTEGER;
",
    "
ALTER TABLE attempts ADD COLUMN first_edit INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN journal INTEGER NOT NULL DEFAULT 1;
",
    "
ALTER TABLE attempts ADD COLUMN input_tokens INTEGER;
ALTER TABLE attempts ADD COLUMN output_tokens INTEGER;
ALTER TABLE attempts ADD COLUMN cache_read_input_tokens INTEGER;
ALTER TABLE attempts ADD COLUMN cache_creation_input_tokens INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN context TEXT NOT NULL DEFAULT '';
ALTER TABLE tasks ADD COLUMN context_enabled INTEGER NOT NULL DEFAULT 1;
",
    "
ALTER TABLE tasks ADD COLUMN resume_on_failure INTEGER NOT NULL DEFAULT 0;
",
    "
CREATE TABLE decisions (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  repo TEXT NOT NULL,
  question TEXT NOT NULL,
  answer TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
",
    "
ALTER TABLE tasks ADD COLUMN plan TEXT NOT NULL DEFAULT '';
",
    "
ALTER TABLE decisions ADD COLUMN answered_by TEXT NOT NULL DEFAULT 'operator';
ALTER TABLE decisions ADD COLUMN citations TEXT NOT NULL DEFAULT '';
ALTER TABLE decisions ADD COLUMN retry_id INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN landed_sha TEXT NOT NULL DEFAULT '';
UPDATE tasks SET landed_sha = substr(reason, instr(reason, '@ ') + 2, 8) WHERE reason LIKE 'landed %' AND instr(reason, '@ ') > 0;
",
    "
CREATE TABLE plugins (
  name TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0,
  enabled_at INTEGER
);
",
    "
CREATE TABLE task_refs (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  kind TEXT NOT NULL,
  url TEXT NOT NULL,
  label TEXT NOT NULL DEFAULT '',
  by TEXT NOT NULL DEFAULT 'operator',
  created_at INTEGER NOT NULL
);
CREATE INDEX task_refs_task ON task_refs(task_id, id);
",
    "
ALTER TABLE attempts ADD COLUMN early_signals TEXT NOT NULL DEFAULT '[]';
ALTER TABLE attempts ADD COLUMN early_near TEXT NOT NULL DEFAULT '[]';
",
    "
ALTER TABLE tasks ADD COLUMN journal_arm TEXT NOT NULL DEFAULT 'treatment';
",
    "
CREATE TABLE projects (
  name TEXT PRIMARY KEY,
  purpose TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  workflow TEXT,
  per_task_usd REAL,
  per_initiative_usd REAL,
  supervisor_model TEXT,
  supervisor_per_lineage INTEGER,
  protected_json TEXT
);
CREATE TABLE project_repos (
  project TEXT NOT NULL REFERENCES projects(name),
  repo TEXT NOT NULL,
  scope_json TEXT,
  PRIMARY KEY (project, repo)
);
CREATE TABLE initiatives (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  outcome TEXT NOT NULL,
  budget_usd REAL,
  stop_after_same_rule INTEGER NOT NULL DEFAULT 3,
  created_at INTEGER NOT NULL,
  settled_at INTEGER
);
CREATE TABLE backlog (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  text TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  done_at INTEGER
);
ALTER TABLE tasks ADD COLUMN project TEXT;
ALTER TABLE tasks ADD COLUMN initiative INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN provider TEXT NOT NULL DEFAULT 'anthropic';
ALTER TABLE attempts ADD COLUMN runner TEXT NOT NULL DEFAULT 'claude-cli';
ALTER TABLE attempts ADD COLUMN provider TEXT NOT NULL DEFAULT 'anthropic';
",
    "
ALTER TABLE projects ADD COLUMN role_providers_json TEXT;
",
    "
CREATE TABLE deploy_targets (
  project TEXT NOT NULL REFERENCES projects(name),
  name TEXT NOT NULL,
  repo TEXT NOT NULL,
  scope_json TEXT,
  method TEXT NOT NULL,
  args_json TEXT NOT NULL DEFAULT '{}',
  check_cmd TEXT NOT NULL,
  on_landing INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (project, name)
);
CREATE TABLE deploys (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL,
  target TEXT NOT NULL,
  sha TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  check_ok INTEGER,
  check_output TEXT NOT NULL DEFAULT '',
  rolled_back_to TEXT,
  reason TEXT NOT NULL DEFAULT ''
);
CREATE INDEX deploys_project_target ON deploys(project, target, id);
",
    "
ALTER TABLE deploys ADD COLUMN task_id INTEGER;
",
    "
ALTER TABLE tasks ADD COLUMN question_to TEXT;
ALTER TABLE decisions ADD COLUMN answered_for TEXT;
",
    "
ALTER TABLE tasks ADD COLUMN explore_json TEXT NOT NULL DEFAULT '{}';
",
    "
ALTER TABLE deploy_targets ADD COLUMN smoke_url TEXT;
ALTER TABLE deploys ADD COLUMN smoke_ok INTEGER;
ALTER TABLE deploys ADD COLUMN smoke_json TEXT;
",
    "
CREATE TABLE task_churn (
  task_id INTEGER PRIMARY KEY REFERENCES tasks(id),
  added_lines INTEGER NOT NULL,
  churned_lines INTEGER NOT NULL,
  computed_at INTEGER NOT NULL
);
",
    // Before ctx::resolve_provider stopped the supervisor from inheriting
    // a task's provider (task 309), a supervisor attempt still recorded
    // the task's model in `inputs_json`, not its own; the provider column
    // is already right (the built-in default, "anthropic"), so these rows
    // read "supervisor anthropic qwen3-coder:30b" instead of the
    // supervisor's own model. Backfill it to the supervisor's default,
    // "opus" (`config::Supervisor::model`'s default) — the only value a
    // migration with no access to the operator's config can give.
    "
UPDATE attempts SET inputs_json = json_set(inputs_json, '$.model', 'opus')
WHERE step = 'supervisor' AND provider = 'anthropic';
",
    // Replaces the path-overlap follow-on cost: `task_repair_cost` caches,
    // per landed task, the git-line-overlap attribution total (the
    // REPAIRCOST column); `line_overlap_cache` caches the per-(T, L) pair
    // git computation it is built from, keyed by both landed commits so
    // it is only ever computed once.
    "
CREATE TABLE task_repair_cost (
  task_id INTEGER PRIMARY KEY REFERENCES tasks(id),
  repair_cost REAL NOT NULL,
  computed_at INTEGER NOT NULL
);
CREATE TABLE line_overlap_cache (
  t_sha TEXT NOT NULL,
  l_sha TEXT NOT NULL,
  overlap_lines INTEGER NOT NULL,
  removed_lines INTEGER NOT NULL,
  PRIMARY KEY (t_sha, l_sha)
);
",
    // The assess directive's own record: one row per landing that ran it
    // (see src/assess.rs), never read by a view or `forge stats` — a
    // fast proxy for a landed task's true cost, kept beside it rather than
    // folded into either.
    "
CREATE TABLE assessments (
  id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(id),
  score INTEGER NOT NULL,
  findings_json TEXT NOT NULL DEFAULT '[]',
  model TEXT NOT NULL,
  provider TEXT NOT NULL,
  cost_usd REAL,
  created_at INTEGER NOT NULL
);
CREATE INDEX assessments_task ON assessments(task_id, id);
",
    // The deploy-look directive's verdict on a deploy's own row, beside
    // smoke_ok/smoke_json (see src/deploy_look.rs, docs/DEPLOY.md, "The
    // deploy look").
    "
ALTER TABLE deploys ADD COLUMN look_ok INTEGER;
ALTER TABLE deploys ADD COLUMN look_json TEXT;
",
    // The customer portal's access token (see docs/PORTAL.md, "What it
    // is"): a per-project link, minted by `forge project portal`. A
    // project can have more than one active token (minting again without
    // `--revoke` just adds one); `revoked_at` is how one stops working.
    "
CREATE TABLE portal_tokens (
  token TEXT PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE INDEX portal_tokens_project ON portal_tokens(project);
",
    // The concierge's decision, raw JSON, on the task it produced (see
    // docs/INTAKE.md, "The front door is not the interview"); an answer
    // records a decisions row instead, nothing here.
    "
ALTER TABLE tasks ADD COLUMN concierge_json TEXT;
",
    // A job is a run of a `kind = "run"` workflow: it starts from a
    // trigger, produces effects, and ends when they're verified — no
    // repository, no branch, no landing (see docs/JOBS.md, "The record").
    // `job_steps` and `job_effects` carry what happened; `jobs` is its own
    // state, cost and verdict.
    "
CREATE TABLE jobs (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  workflow TEXT NOT NULL,
  workflow_hash TEXT NOT NULL DEFAULT '',
  landed_sha TEXT NOT NULL DEFAULT '',
  trigger_kind TEXT NOT NULL,
  trigger_ref TEXT NOT NULL DEFAULT '',
  state TEXT NOT NULL,
  dry_run INTEGER NOT NULL DEFAULT 0,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  cost_usd REAL,
  verdict_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX jobs_project ON jobs(project, id);
CREATE INDEX jobs_state ON jobs(state, id);
CREATE TABLE job_steps (
  id INTEGER PRIMARY KEY,
  job_id INTEGER NOT NULL REFERENCES jobs(id),
  seq INTEGER NOT NULL,
  action TEXT NOT NULL,
  kind TEXT NOT NULL,
  provider TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  cost_usd REAL,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  exit_code INTEGER,
  output_ref TEXT NOT NULL DEFAULT ''
);
CREATE INDEX job_steps_job ON job_steps(job_id, seq);
CREATE TABLE job_effects (
  id INTEGER PRIMARY KEY,
  job_id INTEGER NOT NULL REFERENCES jobs(id),
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL,
  target TEXT NOT NULL,
  summary TEXT NOT NULL DEFAULT '',
  dry_run INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX job_effects_job ON job_effects(job_id, seq);
",
    // The escalator (see docs/INTAKE.md, "The escalator"): the pattern a
    // concierge decision named, on the placeholder task `forge ask` blocks
    // with the yes/no question, and how it was answered.
    "
ALTER TABLE tasks ADD COLUMN proposal_json TEXT;
ALTER TABLE tasks ADD COLUMN proposal_answer TEXT;
ALTER TABLE tasks ADD COLUMN proposal_initiative INTEGER;
",
    // The day tasks are filed in a customer's own words (see
    // docs/PORTAL.md): `forge add --title` and the concierge, on a filed
    // request, both set this; everything before it is NULL.
    "
ALTER TABLE tasks ADD COLUMN title TEXT;
",
    // Where a job's workflow was resolved from (see docs/JOBS.md, "Where
    // an automation lives"): the project's own repository at its pinned
    // commit, or the operator's catalog when the repository had no
    // workflow of that name there. `'catalog'` is the default so every
    // job recorded before this column existed reads as it always ran.
    "
ALTER TABLE jobs ADD COLUMN workflow_source TEXT NOT NULL DEFAULT 'catalog';
",
    // Two metrics the record did not compute on its own: human attention
    // (what a person had to do for a piece of landed work) and time to
    // live (how long a request took to go live). `landed_at` is when a
    // task actually landed — distinct from `finished_at`, which for a
    // task verified before it had anywhere to land (or queued `--no-land`)
    // is set at verification time, before a human's later `forge land`
    // (see docs/LATER.md's "Two metrics"). `hand_landed` is set only by
    // that hand path (`forge land`), never by the supervisor's own
    // automated landing. `task_hand_commits` caches, per landed task, the
    // hand commits (author not Forge's identity) on the base branch
    // between the previous landing on the same repository and this one's
    // `base_sha` — the git-level number `refresh_hand_commits` in view.rs
    // computes once and never revisits, since neither endpoint of that
    // range ever changes once this task has landed.
    "
ALTER TABLE tasks ADD COLUMN landed_at INTEGER;
ALTER TABLE tasks ADD COLUMN hand_landed INTEGER NOT NULL DEFAULT 0;
CREATE TABLE task_hand_commits (
  task_id INTEGER PRIMARY KEY REFERENCES tasks(id),
  hand_commits INTEGER NOT NULL,
  computed_at INTEGER NOT NULL
);
",
    // Indexes for the queries the portal and the initiative report run
    // per render (docs/REVIEW-2.md, item 5): tasks by project and by
    // initiative, decisions and deploys by task, backlog and repositories
    // by project.
    "
CREATE INDEX tasks_project ON tasks(project, id);
CREATE INDEX tasks_initiative ON tasks(initiative, id);
CREATE INDEX decisions_task ON decisions(task_id, id);
CREATE INDEX deploys_task ON deploys(task_id, id);
CREATE INDEX backlog_project ON backlog(project, id);
CREATE INDEX project_repos_project ON project_repos(project);
",
    // A schedule trigger's slot (docs/JOBS.md, "Triggers"): one job per
    // project, workflow and slot, so a tick that reconsiders an already-
    // started slot (a second worker pass before the next one comes due, a
    // restart replaying the same tick) fails the insert instead of
    // starting a second job for it. Manual and other triggers share the
    // same `trigger_ref` column but are never unique on it, so the index
    // is partial.
    "
CREATE UNIQUE INDEX jobs_schedule_slot ON jobs(project, workflow, trigger_ref) WHERE trigger_kind = 'schedule';
",
    // A delayed job (docs/JOBS.md, "Delayed jobs"): `due_at` is the unix
    // second `claim_next_job` compares against now before a `scheduled`
    // job is claimable. The wait is this column, never an in-memory timer,
    // so it survives a worker restart. NULL for every job that was never
    // delayed, including every one recorded before this column existed.
    "
ALTER TABLE jobs ADD COLUMN due_at INTEGER;
",
    // The message record (see docs/PLUGINS.md): one row per message on a
    // channel, in either direction, so a rule can ask "has this contact
    // replied since" (a `[skip_if]` reading `forge message list --json`)
    // without a channel plugin keeping its own log. `task_id` links a
    // message to the task it was about, when there is one (a concierge
    // exchange, an intake interview's question); NULL otherwise.
    "
CREATE TABLE messages (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  channel TEXT NOT NULL,
  contact TEXT NOT NULL,
  direction TEXT NOT NULL,
  text TEXT NOT NULL,
  at INTEGER NOT NULL,
  task_id INTEGER REFERENCES tasks(id)
);
CREATE INDEX messages_project ON messages(project, id);
CREATE INDEX messages_contact ON messages(project, contact, at);
",
    // `[limits] on_failure = \"retry:N\"` (docs/JOBS.md, \"The human rung\"):
    // how many times a job's lineage has already been requeued with the
    // same input, so `job::apply_on_failure` can stop once it reaches `N`
    // rather than retrying forever. 0 for every job recorded before this
    // column existed, and for every original (non-retry) run.
    "
ALTER TABLE jobs ADD COLUMN retry_count INTEGER NOT NULL DEFAULT 0;
",
    // A message trigger's cause (docs/JOBS.md, \"Triggers\"): one job per
    // project, workflow and message id (`trigger_ref`), so recording the
    // same message's trigger a second time fails the insert instead of
    // starting a second job for it — the schedule slot's index, for a
    // message. Partial for the same reason: manual and schedule jobs
    // share the column and are never unique on it here. A `retry:N`
    // requeue keeps its job's `trigger_ref` (docs/JOBS.md, \"The human
    // rung\") and has a `retry_count` above 0, so it is exempt: only the
    // firing itself is unique.
    "
CREATE UNIQUE INDEX jobs_message_ref ON jobs(project, workflow, trigger_ref) WHERE trigger_kind = 'message' AND retry_count = 0;
",
    // Per-hook webhook tokens (docs/JOBS.md, \"Triggers\"): `forge project
    // webhook token` mints one for a project's webhook `name`, `revoke`
    // sets `revoked_at`, and `forge job fire` refuses a token that is not
    // an active one for the hook it fires. Only the SHA-256 of the token
    // is kept (`token_hash`), never the token.
    "
CREATE TABLE webhook_tokens (
  id INTEGER PRIMARY KEY,
  project TEXT NOT NULL REFERENCES projects(name),
  name TEXT NOT NULL,
  token_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE UNIQUE INDEX webhook_tokens_hash ON webhook_tokens(token_hash);
CREATE INDEX webhook_tokens_hook ON webhook_tokens(project, name);
",
    // A webhook trigger's cause (docs/JOBS.md, \"Triggers\"): one job per
    // project, workflow and delivery key (`trigger_ref`: the caller's
    // `--ref`, else a hash of the input), so a delivery its sender retries
    // — or two of them racing — fails the insert instead of starting a
    // second job. The message index's shape, for a webhook.
    "
CREATE UNIQUE INDEX jobs_webhook_ref ON jobs(project, workflow, trigger_ref) WHERE trigger_kind = 'webhook' AND retry_count = 0;
",
    // The event trigger's place (docs/JOBS.md, \"Triggers\"): per project and
    // run workflow, the byte offset in `events.jsonl` up to which the
    // worker has already examined events, so a restart reads on from there
    // instead of from the start. And its cause: one job per project,
    // workflow and event offset (`trigger_ref`), so an event examined twice
    // — the cursor write lost to a crash — fails the insert instead of
    // starting a second job. The message index's shape, for an event.
    "
CREATE TABLE event_cursors (
  project TEXT NOT NULL REFERENCES projects(name),
  workflow TEXT NOT NULL,
  event_offset INTEGER NOT NULL,
  PRIMARY KEY (project, workflow)
);
CREATE UNIQUE INDEX jobs_event_ref ON jobs(project, workflow, trigger_ref) WHERE trigger_kind = 'event' AND retry_count = 0;
",
    // What an operation step printed (docs/JOBS.md, \"The executor\"): the
    // last lines of its stdout and stderr, on the step's own row, so a job
    // that failed on a step says why without the scratch tree in hand.
    "
ALTER TABLE job_steps ADD COLUMN tail TEXT NOT NULL DEFAULT '';
",
    // Task shape at intake (see docs/ECONOMIST.md, "Task shape"): what the
    // economist must condition on before a task even runs, recorded once
    // at enqueue (`queue::task_shape`) rather than derived later from
    // fields that can drift (a workflow file can change; a task's own
    // text never does). `shape_declared_checks` is the repository's own
    // `[checks]` count at that moment; the others are read off the task's
    // text and its resolved workflow. `migrate` backfills every existing
    // task's first three columns from what its row already carries
    // (`tasks::backfill_task_shape`) when it applies this entry;
    // `shape_declared_checks` has no such source and stays 0 for them.
    "
ALTER TABLE tasks ADD COLUMN shape_text_len INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN shape_path_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN shape_tdd INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN shape_declared_checks INTEGER NOT NULL DEFAULT 0;
",
    // The routing record (see `Task::routing`, docs/ECONOMIST.md).
    "
ALTER TABLE tasks ADD COLUMN model_source TEXT NOT NULL DEFAULT 'default';
ALTER TABLE tasks ADD COLUMN workflow_source TEXT NOT NULL DEFAULT 'default';
ALTER TABLE tasks ADD COLUMN routing_json TEXT NOT NULL DEFAULT '{}';
",
    // `forge stats --reprice` (docs/ECONOMIST.md, "Repricing a
    // free-reporting provider"): when this ran on an attempt, so a rerun
    // without `--force` skips it even once its `cost_usd` is no longer 0
    // or NULL (see `Store::reprice_attempts`). NULL for every attempt
    // never repriced, including every one recorded before this column
    // existed.
    "
ALTER TABLE attempts ADD COLUMN repriced_at INTEGER;
",
    // `decisions.task_id` becomes nullable: `forge stats --reprice`
    // records its run as a decision row (docs/SUPERVISOR.md, "Every
    // answer is a decision row"), but a reprice run answers no task's
    // question and touches attempts across many tasks, so it has none to
    // name. SQLite has no `ALTER COLUMN`, so the table is rebuilt;
    // `decisions_task` is recreated after, same as it was defined
    // (`CREATE INDEX decisions_task ON decisions(task_id, id)`, above).
    "
CREATE TABLE decisions_new (
  id INTEGER PRIMARY KEY,
  task_id INTEGER REFERENCES tasks(id),
  repo TEXT NOT NULL,
  question TEXT NOT NULL,
  answer TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  answered_by TEXT NOT NULL DEFAULT 'operator',
  citations TEXT NOT NULL DEFAULT '',
  retry_id INTEGER,
  answered_for TEXT
);
INSERT INTO decisions_new (id, task_id, repo, question, answer, created_at, answered_by, citations, retry_id, answered_for)
  SELECT id, task_id, repo, question, answer, created_at, answered_by, citations, retry_id, answered_for FROM decisions;
DROP TABLE decisions;
ALTER TABLE decisions_new RENAME TO decisions;
CREATE INDEX decisions_task ON decisions(task_id, id);
",
];
