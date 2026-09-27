# Fourth architectural review: the weekly measurement caught up with itself (2026-09-27)

`.forge/workflows/engineering-weekly.toml` fired on schedule and found what
it exists to find: a src file over its 3000-line bound,
`src/workflows.rs` at 3671. The third review, a themed cold read of the
concurrent paths, landed the day before this one fired (2026-09-26); the
last measurement-shaped review was the second, on 2026-09-18, when the
tree stood at 41,211 kernel lines in 44 files. In the nine days between,
513 commits landed, unevenly (48 on the 21st, 156 on the 22nd, 11 on the
24th), and the kernel grew to 69,589 lines. This week's numbers: the five
largest files are workflows.rs (3671), agent.rs (3471), job.rs (2930),
verify.rs (2178) and engine.rs (1987); the eight longest functions run
from `engine.rs`'s `run_directive_step` at 533 lines down to
`view/projects.rs`'s `portal_doc` at 275; 36 modules carry no
`#[cfg(test)]` block; 561 unit and 377 e2e tests run the suite in 83
seconds; clippy is clean at 0 warnings. This review reads the file that
crossed, and the job that caught it, and records only what was confirmed
at its line.

## 1. Findings confirmed while reading

1. **workflows.rs is already half split, and what's left divides the
   same way the first half did.** `mod edges`, `judgment`, `library` and
   `shadow` (workflows.rs:21-24) already carry 798 lines out of the file
   in four pieces with no behaviour shared between them. What remains,
   2522 non-test lines (1-2522) plus a 1149-line `mod tests`
   (2523-3671, 31% of the file), still divides the same way: the
   schema and its parsing (`Contract` through `parse_workflow`,
   38-1341, including the two built-in catalogs and `parse_action` at
   171 lines and `parse_workflow` at 116); the filesystem catalog
   loader (`Catalog`, `load_catalog`, `load_actions`, `load_all`, `get`,
   1441-1576); composition and resolution (`splice`, `check_flow`,
   `resolve`, `job_steps_into`, `resolve_job`, `resolve_job_in_repo`,
   1578-1905); reading a workflow from a *pinned git commit* rather
   than the live catalog (`ls_tree_dir`, `show_at`, `load_all_at`,
   `resolve_job_at`, `resolve_job_for_project`, `fixtures_at`,
   1912-2085) — a genuinely separate dependency (git plumbing) from the
   fs-catalog block above it; and the authoring-time checks
   (`validate_repo`, `lint_catalog`, `lint`, `resolve_jobs_in_tree`,
   `check`, `uncommitted`, `commit_for`, 2155-2520). None of the five
   calls into another's internals beyond the shared `Workflow` and
   `ActionDef` types, the same property that let edges, judgment,
   library and shadow move cleanly.

2. **The job that caught this crossing does not act on seven of the
   eight numbers it measures.** `measure.sh` writes `longest_functions`
   into `measurements.json` (an awk brace-depth scan over every `fn` in
   `src/`), but `compare-thresholds.sh` never reads that field: it
   recomputes `run_task`'s own length with a second, independent awk
   pass over `src/engine.rs` and compares only that number against
   `RUN_TASK_MAX_LINES`. `run_task` is 171 lines (engine.rs:188-358),
   comfortably inside its 400-line bound, four measurement cycles after
   docs/REVIEW-2.md's stage 4 shrank it from 1082. But
   `run_directive_step` — the function that stage 4 deliberately left
   holding "the step loop" when it carved `retry_start`, `escalate` and
   `finish` out of `run_task` — is now 530 lines (engine.rs:801-1330;
   measurements.json's own figure, 533, agrees to within the doc
   comment above it), the single largest function in the kernel, ahead
   of `supervisor.rs`'s `supervise` (460) and `job.rs`'s `run_now`
   (452). All three are measured every week and none is compared
   against anything.

3. **Two of the four modules already split out of workflows.rs carry no
   `#[cfg(test)]` of their own, and the measurement can't tell that from
   genuinely untested code.** `modules_without_tests` names
   `src/workflows/edges.rs` and `src/workflows/shadow.rs` (spot-checked:
   confirmed, alongside `src/agent/inputs.rs`, `src/cli/demo.rs`,
   `src/env_supervisor.rs` and `src/successor.rs` from the same list).
   Both are exercised indirectly through workflows.rs's own suite —
   `inline_composition_splices_and_rejects_cycles` (workflows.rs:2708)
   drives `edges::resolve`'s cycle rejection, `builtins_resolve_and_
   carry_blob_hashes` (workflows.rs:2587) drives `shadow::text_blob_
   hash` through `parse_action` — but a `grep -L '#\[cfg(test)\]'`
   measurement has no way to see that, so the same two names will
   reappear on this list every week even if they are never the reason a
   bug ships.

4. **Two more of the five largest files carry the same shape as
   workflows.rs, and one is a single crossing away.** agent.rs (3471)
   is 38% inline test (`mod tests` from line 2155, 1317 of 3471 lines);
   job.rs (2930) is roughly a fifth (`mod tests` from line 2367). The
   `kernel_lines` and `largest_files` measurements count both in the
   same column as parsing and control flow, so part of what crossed
   this week's threshold is 561 unit tests earning their keep, not new
   logic — worth knowing before a split is sized, not a reason to skip
   one.

## 2. A number this review can't explain

`e2e_wall_time_secs` is 83. The second review's suite ran 195 e2e tests
in 22 seconds; its outcome, after 105 more tests landed the same day,
still ran in 24. This week's suite, 377 e2e tests, takes over three
times as long per test as either of those points. Nothing in this read
found the cause — it would take profiling the suite, not reading
workflows.rs, to say whether it's the sandboxed checks the second review
already named as real host dependencies (bwrap, loopback servers,
headless Chromium) growing in number, a slower shared fixture, or
something waiting on a timer that used to return sooner. Recorded so the
next reader has the baseline and isn't the first to notice it.

## 3. Keep

- **`run_task` held its own bound.** Four measurement cycles after
  docs/REVIEW-2.md's stage 4, it is 171 lines against a 400-line cap,
  the one function the weekly job actually watches.
- **Clippy is clean.** 0 warnings at `-D warnings`, same as every prior
  review.
- **The split workflows.rs already did holds up.** edges.rs,
  judgment.rs, library.rs and shadow.rs have not grown back into
  workflows.rs; nothing in this read found a caller reaching around
  them into workflows.rs internals or vice versa.
- **The measurement itself is sound where it's read.** `largest_files`,
  `modules_without_tests` and `clippy_warnings` all matched what this
  read confirmed by hand; the gap is that `longest_functions` is
  computed and then set aside (finding 2).

## 4. The plan

Ordered so stage 0 is cheap and stops the same gap from reopening next
week.

**Stage 0. Make the job compare what it measures (hand).**
`compare-thresholds.sh` should read `.longest_functions[]` from
`measurements.json` instead of re-deriving `run_task`'s length with its
own awk pass, and gain a `FUNCTION_MAX_LINES` `[env]` default (400, matching
`RUN_TASK_MAX_LINES`) so any function over the bound names itself in the
filed task, `run_task` included. `run_directive_step` should cross it on
the next run without anyone reading engine.rs first.

**Stage 1. run_directive_step's own seams (hand, after 0 or in
response to it).** Split the same way `run_task` was split in
docs/REVIEW-2.md's stage 4: the per-step attempt loop, the
provider-hold wait, and the refund and rewind bookkeeping into named
functions or `StepFlow` variants, each unit-tested on its inputs, target
a few hundred lines. The discipline already exists once; this is
applying it a second time to the piece that inherited the first
function's size.

**Stage 2. workflows.rs's remaining five (Forge, after 0 so the guard
exists to hold the result). One task per concern, no behaviour change,
each one's own tests moving with it (unlike edges.rs and shadow.rs,
whose tests stayed behind — finding 3): schema and parsing into
`workflows/schema.rs`; the filesystem catalog loader into
`workflows/catalog.rs`; composition and resolution
(`splice`/`check_flow`/`resolve`/`job_steps*`/`resolve_job*`) into
`workflows/compose.rs`; the pinned-commit git reader
(`ls_tree_dir`/`show_at`/`load_all_at`/`resolve_job_at`/`fixtures_at`)
into `workflows/pinned.rs`; the authoring-time checks
(`validate_repo`/`lint`/`resolve_jobs_in_tree`/`check`) into
`workflows/lint.rs`. Add a size guard beside the split, the way cli.rs's
function-length test guards its own stage 3, so the next feature can't
silently rebuild the monolith one workflow concern at a time.

**Stage 3. A handful of the 36 (Forge, optional, low cost).** Most of
`modules_without_tests` are pure-function modules already exercised
through an e2e path, the same finding docs/REVIEW-2.md's stage 6 made
before adding tests to eight of them. Pick the ones with real branching
(not `src/workflows/edges.rs` or `shadow.rs` — finding 3 says why not
yet) and give them the same treatment, no new fakes.

## 5. What this buys

Stage 0 is the smallest fix that matters most: it turns a measurement
that already runs every week into a check that catches the next
`run_directive_step` before a reader has to find it by hand. Stage 1
returns the run's step loop to the size the second review's stage 4 put
it at. Stage 2 finishes what edges, judgment, library and shadow started,
with a guard so it stays finished. None of it changes what a workflow or
an action does; every stage is checked by the same 561 unit and 377 e2e
tests.
