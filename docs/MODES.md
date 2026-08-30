# Modes

A **Mode** is the unit of "what kind of work is this". It fixes the prompt scaffold, the
tools the agent may use, the shape of the result it must return, the verification level
Forge applies afterwards, the checkpoints at which lower autonomy levels pause, and the
default budget class and autonomy.

```go
// internal/modes
type Mode interface {
    Name() string
    // PromptTemplate is the embedded default; the live copy is ~/.forge/modes/<name>.md,
    // seeded by `forge init`, and is what actually runs (so proposals can target it).
    PromptTemplate() string
    AllowedTools() []string          // Claude built-ins and forge_* tools
    ResultSchema() json.RawMessage   // passed via --json-schema
    VerificationLevel() verify.Level // required level: L0 (the minimum), L1, L2, or L3
    Checkpoints() []string           // names; empty for none
    DefaultBudgetClass() model.BudgetClass
    DefaultAutonomy() model.Autonomy
    Writes() WriteScope              // None | KbOnly | DocsOnly | Repo | NewProject
    // FollowUps is the one place a mode turns its result into more Work: the
    // intake → implement handoff, and the verify Work an L2 mode needs.
    FollowUps(result Result) []WorkSpec
}
```

Modes are registered in `cmd/forge` on a registry value (`registry.Register(run.New())`).
Adding a mode is one package under `internal/modes/<name>/` (the mode file, its
embedded default prompt, and its schema) plus one `Register` call.

## Prompt assembly

Every attempt's prompt is rendered from three layers, in this order:

1. **Mode preamble** (`~/.forge/modes/<mode>.md`) — the mode's rules, the result
   contract, and the autonomy instructions for the level in force.
2. **Routine prompt** — the routine's `prompt` with `{{repo}}` substituted (and, for
   scheduled Work, a line with the occurrence time).
3. **Context block** — Forge-computed facts the agent should not have to discover:
   repository name, base branch and commit, worktree path, the declared checks from
   `forge.toml`, the autonomy level, remaining budget headroom, and the attempt ID.

The rendered prompt goes to the executor on stdin. The `PromptVersion` hash covers
layers 1 and 2 as **templates** (before substitution), plus the system append, sorted
tool list, model, and effort — never layer 3 or the substituted values, which differ
per attempt and would defeat A/B comparison (`DESIGN.md` §9.1).

**Autonomy instructions** rendered into the preamble:

| Level | Instruction |
|---|---|
| `ask` | "When anything is ambiguous, before any irreversible step (commit, deleting a file, changing a dependency), or if you expect to exceed the cost estimate by 50%, stop and return `needs_input`." |
| `checkpoint` | "At each of these checkpoints — *list* — stop and return `needs_input` with `checkpoint` set. Otherwise decide and proceed." |
| `notify` | "At each checkpoint call `forge_note_progress` with `checkpoint` set and continue. Decide and proceed on ambiguity." |
| `auto` | "Decide and proceed. Never return `needs_input`." (A `needs_input` at `auto` is a failure, reason `ambiguity_at_auto`.) |

## Result contract

Every mode's `--json-schema` is the common envelope plus mode-specific fields. The
envelope:

```json
{
  "schema_version": 1,
  "summary": "one paragraph, plain text",
  "needs_input": null,
  "changes":    [{"path": "…", "kind": "added|modified|deleted", "summary": "…"}],
  "checks_run": [{"check": "build|test|lint|<name>", "passed": true, "notes": "…"}],
  "claims":     [{"claim": "…", "evidence": "…"}]
}
```

`needs_input`, when not null:

```json
{"question": "…", "options": ["…"], "context": "…", "checkpoint": "after_spec|null"}
```

Rules Forge enforces on the envelope (part of L0, see `VERIFICATION.md`):

- `changes[]` must agree with `git` — every path listed changed in the worktree, and
  every changed path is listed. Modes with `Writes() == None` must have an empty
  `changes[]` *and* a clean, commit-free worktree.
- `checks_run[]` is a claim. L1 re-runs the same checks; a mismatch is `unverified`.
- `claims[]` are free-text but each must carry `evidence`; a `verify`-mode attempt
  reads them.
- The agent never reports counts, durations, or costs; Forge ignores any it includes.

## Tool naming

Claude built-ins are named as Claude names them (`Read`, `Edit`, `Bash`, …; `Bash(git
diff:*)` patterns are allowed). Forge tools are served by `forge mcp` under the MCP
server name `forge`, so in `--allowedTools` they appear as `mcp__forge__forge_usage`
etc. Mode definitions list them as `forge_usage`; the executor adds the prefix. Modes
that must have **no** built-in tools pass `--tools ""` in addition to `--allowedTools`.

Every mode gets `forge_note_progress`, `forge_kb_search`, and `forge_usage`. Every mode
at autonomy `ask`/`checkpoint` also gets `forge_ask`. `forge_ask` **does not block**:
it records the Question and returns immediately with the instruction to end the turn.
The one rule (`DESIGN.md` §4.1) is "an open Question exists when the process exits ⇒
`waiting_human`", whether it came from the tool or from `needs_input`; the agent is
told to prefer `needs_input`, and to call the tool only when it wants the question
recorded before finishing its turn.

## The modes

### `run`

One prompt, one repository, current context. The routine prompt *is* the task.

- **Prompt outline:** preamble (rules: stay in the worktree; commit if the task asks;
  never push) → routine prompt → context block.
- **Tools:** all built-ins; `forge_repo_status`, `forge_check`, `forge_diff_summary`.
- **Result:** envelope only.
- **Verification:** L1 (declared checks re-run, if the repo declares any; L0 otherwise).
- **Checkpoints:** none. **Budget class:** `normal`. **Autonomy:** project default.
- **Writes:** `Repo`.

### `greenfield`

Interview → spec → plan → build → verify → report, across several sessions.

- **Prompt outline:** phase-driven. Phase 1 *interview*: ask why, for whom, non-goals,
  acceptance criteria — via `needs_input` (one question per turn; at `auto`, infer and
  state assumptions). Phase 2 *spec*: write a kb note of type `spec` with `forge_kb_new`
  → checkpoint `after_spec`. Phase 3 *plan*: ordered steps with acceptance checks →
  checkpoint `after_plan`. Phase 4 *build* in the new project directory; initialise git;
  commit per step. Phase 5 *verify*: declare checks in a `forge.toml`, run them.
  Phase 6 *report*.
- **Tools:** all built-ins; `forge_kb_new`, `forge_kb_note`, `forge_check`.
- **Result:** envelope + `spec_note`, `plan[] {step, done, evidence}`, `project_path`.
- **Verification:** L2 (a `verify` attempt exercises the built thing).
- **Checkpoints:** `after_spec`, `after_plan`, `before_report`. **Budget class:**
  `normal`. **Autonomy:** `checkpoint`.
- **Writes:** `NewProject`. **Open question for the human (M0 report):** the project
  name comes out of the interview, so the directory cannot exist before the agent
  starts, and a directory outside `data_dir` breaks the manifest's owned-path rule.
  Recommended design, pending decision: the attempt runs in
  `<data_dir>/greenfield/<attempt-id>` (Forge `git init`s it in `worktree_add`; the
  manifest records `kind = greenfield`); the Target's `repository_name` is the
  virtual `greenfield`; `fetch`/`resolve_base` are skipped; at `before_report` the
  result carries `project_name`, and on completion Forge moves the directory to
  `projects_root/<slug>` (refusing, and retaining in place, if the destination
  exists) and records the final path on the attempt. Continuation sessions run in the
  recorded path with `--resume`. Cleanup never removes either location.

### `intake`

Bug report or feature request → reproduce or refine → issue text with acceptance
criteria → optional handoff.

- **Prompt outline:** read the request; locate the relevant code; reproduce (for bugs)
  or refine (for features) without editing tracked files; produce an issue body with
  *Summary / Reproduction or Motivation / Acceptance criteria / Out of scope*; write it
  as a kb note (type `note`, tag `intake`); checkpoint `before_handoff`; if handoff is
  wanted, set `handoff.requested = true`.
- **Tools:** `Read`, `Grep`, `Glob`, `Bash`; `forge_kb_new`, `forge_kb_search`,
  `forge_repo_status`.
- **Result:** envelope + `issue {title, body, acceptance_criteria[]}`, `reproduced`
  (bool|null), `handoff {requested, mode: "implement"}`, `note_id`.
- **Verification:** L0 with `Writes() == KbOnly` (worktree must be clean).
- **Checkpoints:** `before_handoff`. Forge creates the handoff Work (`trigger:
  dependency`, mode `implement`, prompt = issue body) when `handoff.requested` and the
  checkpoint is passed or autonomy is `notify`/`auto`.
- **Budget class:** `normal`. **Autonomy:** `checkpoint`. **Writes:** `KbOnly`.

### `implement`

Issue → branch → commits with verification evidence.

- **Prompt outline:** understand the issue; plan briefly; implement in small commits;
  run the declared checks after each meaningful step and before finishing; record each
  run in `checks_run`; checkpoint `before_report`; final summary names commits.
- **Tools:** all built-ins; `forge_check`, `forge_repo_status`, `forge_diff_summary`,
  `forge_kb_search`.
- **Result:** envelope + `commits[] {sha, subject}`, `tests_added[]`.
- **Verification:** L2 (L1 checks, then a `verify` attempt).
- **Checkpoints:** `before_report`. **Budget class:** `normal`. **Autonomy:** project
  default. **Writes:** `Repo`.

### `review`

PR or diff → structured findings. Never writes.

- **Prompt outline:** obtain the diff (`forge_diff_summary`, `git diff`); for each hunk
  look for correctness, missing tests, style-rule violations, and risk; rank findings;
  no edits.
- **Tools:** `Read`, `Grep`, `Glob`, `Bash(git diff:*)`, `Bash(git log:*)`,
  `Bash(git show:*)`; `forge_diff_summary`, `forge_repo_status`, `forge_kb_search`.
- **Result:** envelope + `findings[] {file, line, severity: "high|medium|low",
  category, summary, suggestion}`, `verdict: "approve|request_changes|comment"`.
- **Verification:** L0, `Writes() == None`. **Checkpoints:** none. **Budget class:**
  `normal`. **Autonomy:** `auto`.

### `verify`

Independently re-check another attempt's claims and produce a verdict. Runs in its own
worktree checked out at the subject attempt's head commit, in a fresh session with no
shared context.

- **Prompt outline:** input is the subject attempt's structured result and `claims[]`
  (Forge renders them into the prompt, never the transcript); for each claim design a
  check, run it (build, run, hit endpoints with `curl`, drive a UI with Playwright via
  `npx playwright`), record evidence; store screenshots under the artifacts directory
  Forge provides (`{{artifacts}}`); return a verdict.
- **Tools:** `Read`, `Grep`, `Glob`, `Bash`; `forge_check`, `forge_attempt`,
  `forge_repo_status`. No `Edit`/`Write` (artifacts are written by Bash to
  `{{artifacts}}`, outside the worktree).
- **Result:** envelope + `verdict: "pass|fail|inconclusive"`, `claims_checked[]
  {claim, result: "confirmed|refuted|unverifiable", evidence, artifact}`.
- **Verification:** L0 with `Writes() == None` (it *is* verification of its subject;
  Forge only asserts it wrote nothing to the worktree).
- **Checkpoints:** none. **Budget class:** `interactive` if the subject's Work was
  `interactive`, otherwise `normal` (a `backlog` subject's verification is not itself
  backlog — the subject is already holding a `verifying` Target). **Autonomy:**
  `auto`. **Writes:** `None` (artifacts dir only).

### `audit`

Scheduled read-only sweep: security, dependencies, dead code, doc drift.

- **Prompt outline:** one pass per concern; each finding has file/line, severity,
  and a suggested action; write a kb note (type `note`, tag `audit`) summarising;
  no edits.
- **Tools:** `Read`, `Grep`, `Glob`, `Bash`; `forge_kb_new`, `forge_kb_search`,
  `forge_repo_status`.
- **Result:** envelope + `findings[]` (as `review`) + `note_id`.
- **Verification:** L0, `Writes() == KbOnly`. **Checkpoints:** none. **Budget class:**
  `backlog`. **Autonomy:** `auto`.

### `maintain`

Dependency bumps, CI fixes, formatting. Must pass checks.

- **Prompt outline:** apply the maintenance the routine names; run declared checks;
  if checks fail, revert and report rather than "fix" unrelated code; commit.
- **Tools:** all built-ins; `forge_check`, `forge_repo_status`, `forge_diff_summary`.
- **Result:** envelope + `commits[]`.
- **Verification:** L1. **Checkpoints:** none. **Budget class:** `backlog`.
  **Autonomy:** project default. **Writes:** `Repo`.

### `docs`

Diff-driven documentation sync.

- **Prompt outline:** read the diff since the last docs run (Forge renders the range);
  find docs that describe changed behaviour; update them; touch nothing outside the
  declared doc paths; commit.
- **Tools:** all built-ins; `forge_diff_summary`, `forge_repo_status`, `forge_check`.
- **Result:** envelope + `docs_updated[]`.
- **Verification:** L1, plus `Writes() == DocsOnly` — every changed path must match
  `[modes.docs] paths` in the repo's `forge.toml` (default `docs/**`, `*.md`).
- **Checkpoints:** none. **Budget class:** `backlog`. **Autonomy:** project default.

### `explore`

Read-only research → kb note.

- **Prompt outline:** answer the routine's question from the code; cite files; write
  one kb note (type `note`, tag `explore`, `about` the repository) with the answer.
- **Tools:** `Read`, `Grep`, `Glob`, `Bash(git log:*)`, `Bash(git show:*)`;
  `forge_kb_new`, `forge_kb_search`, `forge_kb_links`, `forge_repo_status`.
- **Result:** envelope + `note_id`, `sources[] {path, why}`.
- **Verification:** L0, `Writes() == KbOnly`. **Checkpoints:** none. **Budget class:**
  `backlog`. **Autonomy:** `auto`.

### `retro`

Data pack in → kb retro note + proposals out. Tools only.

- **Prompt outline:** call `forge_retro_pack` for the window; for each routine compare
  the current generation with the previous window; list what went well / what did not,
  each tied to a metric and its delta; form hypotheses; write one kb note of type
  `retro` via `forge_kb_new`; for each hypothesis worth acting on, call `forge_propose`
  with `before`, `after`, `rationale`, and a `verification_plan`. Never propose changes
  to the constitution. Never count — every number comes from the pack.
- **Tools:** `--tools ""` (no built-ins); `forge_retro_pack`, `forge_stats`,
  `forge_attempt`, `forge_events`, `forge_prompt_version`, `forge_usage`,
  `forge_queue`, `forge_kb_search`, `forge_kb_new`, `forge_kb_note`,
  `forge_kb_backlinks`, `forge_kb_links`, `forge_propose`, `forge_note_progress`.
- **Result:** envelope + `note_id`, `proposals[] {id, kind, target}`,
  `hypotheses[] {statement, metric, expected_delta}`.
- **Verification:** L0 with `Writes() == KbOnly` — the attempt runs in a worktree of
  the `forge` repository (registered by `forge init`, `DESIGN.md` §7.1) purely for a
  cwd; nothing may be written there.
- **Checkpoints:** none. **Budget class:** `backlog`. **Autonomy:** `auto`.
  **Writes:** `KbOnly` (+ proposals).

## Checkpoint mechanics

A checkpoint is a named point in a mode's prompt. Its effect depends on autonomy:

- `ask` and `checkpoint`: the agent ends its turn with `needs_input.checkpoint = <name>`
  (options: `["continue", "stop"]` plus anything mode-specific). Forge writes a
  Question, moves the Target to `waiting_human`, frees the slot, keeps the worktree and
  session. The answer resumes with `--resume`.
- `notify`: the agent calls `forge_note_progress` with `checkpoint = <name>`; Forge
  records a lifecycle event and an attention item on the dashboard; the agent continues.
- `auto`: nothing.

The set of checkpoint names is declared by the mode and validated when a mode prompt
file is loaded: a prompt that mentions a checkpoint the mode does not declare is
rejected with the name.
