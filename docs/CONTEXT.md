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
