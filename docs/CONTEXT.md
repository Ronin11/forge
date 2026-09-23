# Context: what the coder is told before it starts looking

## Status (2026-09-13)

Built: `history` as the journal (`forge journal`), and `repo-map` as the
`forge-repomap` tool behind a `context`-producing operation in every
workflow with a coder. Not built: the hand-written conventions file, and
seeding, which the evidence argued against. Both sources have a control
arm (`--no-journal`, `--no-context`) and are measured by calls before the
first edit, attempts, and cost per piece of work.

The map is incremental by construction. Symbols are cached per blob in a
shared, content-addressed store under `FORGE_HOME/cache/repomap` (one
small file per blob, written atomically, so parallel tasks never fight),
and every clone of a repository reads and seeds the same store. A task's
map therefore costs parsing only the blobs its branch or the latest
landings introduced; landing warms the store for free, because the last
map on the branch already parsed the tree that becomes the base. There
is no refresh step to schedule and nothing to keep in the repository.
The tool also takes `--changed-since <base>`: files the branch has already
changed rank first, which is what a retry or a later attempt wants.

The design work that led here, including the unbuilt `history` operation
and its per-step token budget, is in docs/LATER.md.

## The order of a prompt (2026-09-22)

Prompt caching serves the longest byte-identical prefix, so every
directive prompt is assembled in three parts whose boundaries are chosen
for it (`src/prompts.rs`):

1. **The fixed preamble** (`prompts::PREAMBLE`): the rules the kernel
   enforces. No task id, branch, workflow name, config path or attempt
   number in it, so it is the same bytes on every launch of every task.
2. **The repo pack** (`prompts::repo_pack`): what depends only on the
   repository at its base. The map ranked without the task's words (the
   `repo-map` operation prints it before a marker), the checks the
   operator re-runs, the verification namespace. Two tasks on the same
   base share it byte for byte; the model writes it to its cache once.
3. **The task frame and the rest** (`prompts::task_frame` onward): the
   branch and base, the workflow, the config path, the files this task's
   own words rank highest (the map's part after the marker, a small
   budget), why the task exists, protected paths, then the directive's
   own sections, the task text, the journal, the attempt note and the
   action's prompt.

Measured on the day it landed with the lean launch already in: a second
task's turn-1 cache write is the numbers reported in the commit that
introduced this section. The unit test
`two_tasks_on_one_base_share_the_preamble_and_the_repo_pack_byte_for_byte`
pins the shared length against the pack and that the shared prefix
names no task.

The C4 prompt reduction is already implemented by `dca182f`: the fixed
preamble went from 2,036 to 1,751 characters (285 fewer, a 14.0% reduction).
These counts measure the decoded `PREAMBLE` string, including spaces and
newlines, before and after that commit. It removed the instructions to
list changed paths, include lockfiles, and describe renames or moves;
the remaining result contract stays intact. The early-feedback note also
no longer asks for a list of paths changed during the session.

The map itself, when ranked with no words, leads with the files that
declare the most and leaves out files that declare nothing, since a map
ranked by nothing has no other signal than what a file carries.

## Line spans and the map factor (2026-09-22)

Every symbol in the map now carries a span, rendered `name@start-end` (since A3, `name@start`; see the end of this file):
the line the declaration starts on and the line before the next
declaration (the last symbol runs to the file's end). It is a cheap span
with no brace matching, and it is enough for what it is for: the map's
heading tells the model to read the ranges it needs with Read
offset/limit and to batch independent Reads and greps into one turn,
against the measured 1.07 tool calls per turn and 66% of tool output
being whole-file reads. When a file's symbols overflow its 220-character
line, trailing symbols are dropped whole; a span is never cut. Cache
entries written before spans existed read as misses and re-parse.

Whether spans help is measured, not assumed: `map` is an experiment
factor (`[factors.map]` in `experiment.toml`, levels `spans` and
`names`; docs/ECONOMIST.md). A task's draw reaches the `repo-map`
operation as `FORGE_MAP_STYLE`, and `forge stats --factors` reports the
`map` levels beside the others with two exploration measures: the tool
call at which the first edit came, and tool calls per turn. Spans are
the default when no draw was made. The factor is readable once each
level has a few dozen landed tasks; at the current pace that is about
a week with the weights at 0.5 each.

## The tools in the sandbox, and `name@start` (A3)

`forge-repomap` is on `PATH` inside a directive attempt's sandbox, not
only for operations: `Forge::open` binds the directory beside the
`forge` binary read-only, and `agent::agent_env` puts that directory in
front of `PATH` for the agent and the checks alike. The model can run
`forge-repomap outline <path>` and `forge-repomap def <name>` instead of
grepping and reading whole files; the e2e test
`a_directive_attempts_sandbox_can_run_forge_repomap_def` runs it from
inside the sandbox.

The repo pack says so in one line beside "Where things are" (shared by
every task, never the task tail): outline lists a file's signatures with
line ranges, def prints one item, use them before grep, Read with
offset/limit before editing. Because outline and def give the ranges,
the map renders `name@start` only (the end was the line before the next
declaration and cost characters on every symbol). Within the same
budgets in `repo-map.toml` and the 220-character file line, trailing
symbols are dropped whole, never cut, so more files fit.

### forge-test in the sandbox, and the count (B4)

`forge-test` sits beside `forge-repomap`, so the same `PATH` entry and
read-only bind put it inside a directive sandbox; the e2e test
`a_directive_attempts_sandbox_can_run_forge_test_against_a_fake_repository`
runs it there against a throwaway repository and gets the second call
from the cache. The repo pack carries one line for every task, the plain
tools arm included (this is not an arm; the cache can only remove
duplicate runs): run tests with `forge-test [filter args]`, it keeps the
full log and never re-runs an unchanged tree.

Each attempt's log is read into `outputs_json.tools.tests`
(`tools::testruns`): `forge-test` calls, cache hits (the result starts
"cached: tree unchanged"), raw test commands that bypassed it (`cargo
test`, `npm test`, `npx vitest`, `pytest`, `go test`), full-suite runs
(no filter, either way), runs with no Edit/Write since the previous run,
and test wall time (call to result). Only the Claude stream is read.
`forge stats --tests [--last N]` prints the means per role over the last
N attempts (default 50) that recorded them, beside the baseline: 3,669
runs, 64% without an edit, 1,251 full-suite runs, 11.3 hours.

## Outline and def experiment (A4)

`[factors.tools]` in `experiment.toml` accepts `outline` and `plain`.
The task draws once, just like `map`; operations receive that draw as
`FORGE_TOOLS_STYLE`. `outline` includes the outline/def instruction in
`repo_pack`, and `plain` omits it. Undrawn tasks default to `outline`.
The line remains in the shared prefix, so tasks in each arm share their
own pack. Both arms still have the tools available.

Each attempt records `outputs_json.tools.exploration` from its log:
`grep_then_ranged_read_chains`, `unedited_read_chars`,
`turns_before_first_edit`, `outline_calls`, and `def_calls`.
A chain is a search call immediately followed by a ranged read call.
Searches include Grep and shell rg/grep; ranged reads include Read with
an offset or limit, and a single `sed -n 'N,Mp' file` command. Characters
are Unicode characters in successful read results, excluding files
edited anywhere in the attempt (including git changes and dirty files).
Structured Read and single-file cat/sed results can be attributed;
arbitrary scripts and compound shell output cannot. Outline/def counts
come from shell invocations, including commands batched with separators.
Turns count assistant messages (deduplicated by message ID) or Codex
`turn.started` events before the turn containing the first structured
edit/file-change event. No observed edit means null, not zero; shell
edits without file-change events cannot identify that turn. Missing or
unsupported logs produce null exploration. Replayed call IDs count once.
