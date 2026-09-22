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

The map itself, when ranked with no words, leads with the files that
declare the most and leaves out files that declare nothing, since a map
ranked by nothing has no other signal than what a file carries.
