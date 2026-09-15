# The supervisor

The rung between a blocked task and the human. When a task stops with a
question, or a review demotes it, a read-only agent on a strong model
reads the repository's record and does one of three things. Each leaves
an artifact the kernel checks; none can reach main.

- **answer**: tell the next attempt what to do, citing what the answer
  rests on. The task is re-queued with the answer in its text, exactly as
  `forge answer` does, and the decision is recorded as the supervisor's.
- **prerequisite**: write the missing work as a task, queue it, and
  re-queue the blocked task behind it.
- **superseded**: the work already landed through another task of the
  repository; cite it as `task N` and the blocked task ends, so nobody
  redoes it.
- **accept** (review demotions only): the demotion names no defect, or
  an approval was written into the demotion field, or the finding is
  not something the task requires. The verified branch lands as it is;
  nothing is rebuilt. A real defect is an answer telling the next
  attempt what to fix.
- **escalate**: leave the question for the human, with a reason. The
  request still shows in `forge requests` and the web page, marked
  escalated. The human's own reply is `forge answer` or `forge retry`,
  both of which re-queue the task — or, when the task was written
  against a stale description, is superseded, or the product decision
  goes the other way, `forge withdraw <id> --reason <text>`, which ends
  it as `withdrawn` instead of spending an attempt re-running work that
  was never going to be done.

## What it reads

The task and its question with what the attempt tried; the lineage's
journal, verdict first; where things are (the repository map); the
repository's last thirty tasks with their states; every decision so far
in this repository, the operator's marked authoritative, each with the
outcome of the task it re-queued; and the backlog, if the repository
keeps one as `TASKS.md`, `BACKLOG.md`, `docs/BACKLOG.md`, or
`.forge/backlog.md`. It runs in the task's own clone and may run
anything there.

## What the kernel holds it to

Its run is an attempt of the task, so its cost and its verdict sit
beside the attempts it rules on. The verdict rows:

- `result-structured`: a ruling in the supervisor's own schema.
- `untouched`: the clone is exactly as it found it.
- `cites-real-things`: at least one citation, and every citation
  resolves: a path in the tree, `task N` for a task of this repository,
  or `decision N` for a decision that exists. Prose does not count.
- `substantive`: an answer, or a prerequisite's task text, of at least
  forty characters.
- `supersedes-with-a-landed-task`: a superseded ruling cites a task of
  this repository that succeeded.

It is told that a re-queued task starts from a fresh clone of the base
branch, so an answer says what to do from scratch and never "commit
what is in the tree". The first live ruling, on task 127, was right on
the substance, cited seven real places, and missed both of those; hence
the sentence and the fourth action.

A ruling that fails any row is an escalation with the failed rows as the
reason, and no decision is recorded. After `per_lineage` supervisor
answers within one piece of work (two by default), the next question
goes to the human regardless: a supervisor that keeps answering the same
piece of work is guessing.

## Why powers, not rank

Every question asked in the first two weeks was settled by a power the
supervisor has: permission to change a test (which the kernel routes to
the tests directive itself), a prerequisite queued first, a defect
split out, or a rule the coder should have known. None needed intent.
The supervisor holds exactly those powers and no more. Intent stays the
human's, and the supervisor is told to escalate rather than guess at it.

## The learning loop

Every answer is a decision row tagged `supervisor`, with the re-queued
task as its outcome. `forge decisions` shows each answer with the state
that task reached. Over time that says which questions the supervisor
settles and which it should have escalated, and it is what a future
system-level rung would be built on.

## Configuration

```toml
[supervisor]
enabled = true
model = "opus"
max_turns = 30
per_lineage = 2
```

`FORGE2_SUPERVISOR=0` turns it off for one process; the e2e suite runs
that way except where a test hands it a fake. `forge supervise <id>`
runs it on a blocked task by hand.

The contract is runner-agnostic: a prompt in, a structured ruling out,
nothing written. Today the only runner is the claude CLI; a second one
for this role (another provider's model) is an adapter that speaks the
same stream, and is noted in LATER.md.
