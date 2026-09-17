# Projects and initiatives

*2026-09-15. The layers above a task, why there are two, and what each
one is for.*

Forge's record is complete at the level of a task and blind one level
up. This week's work came in units that were real to the person running
it, "stage 9", "the plugin ports", "equitizr's first batch", and none
of them existed in Forge: each was a hand-written shell monitor for a
set of task ids and a rollup written by hand in the morning. The queue
right now holds seventeen tasks with nothing saying which belong
together, the statistics are per workflow, the web list is flat, and the
supervisor reads every decision ever made regardless of what it is
ruling on. One level up is where an operator actually works, and two
levels up is where the work is owned.

This document adds those two levels and nothing else.

## The layers

```
project        what is being built, and for whom
  initiative   one outcome, pursued as a set of tasks
    task       one verified piece of work
      attempt  one run of one agent against one contract
```

Repositories are not a layer. A project references repositories; a
repository can serve several projects; a project can span several
repositories. Forge's own development is a project like any other,
named `forge`, so that what Forge spends on itself is always separable
from what it spends on products.

## Project

A project is the unit of ownership, and later the unit of tenancy.

**Identity and purpose.** A name and one paragraph saying what the
project is for. The paragraph is for people and for the supervisor; it
is not pasted into task prompts. A project the migration or `forge add`
created for a repository nobody had named a project for yet has nothing
real to say, so it gets a placeholder purpose, `Repository <path>.`;
`forge project show`, the portal document, and `forge doctor` (a
warning per project) all treat that placeholder as though no purpose
were set, until `forge project set --purpose <text>` replaces it.

**Repositories and scopes.** A project lists the repositories it works
in, and for each, optionally, the paths it owns within that repository.
This is the monorepo answer: a project is a set of repository-and-paths
pairs, and every task in the project inherits the scope as its
`--paths`, so the paths-in-scope rule that already runs at L0 enforces
the boundary on every attempt. A repository with no scope means the
whole repository. Per-scope checks (one package's tests are not
another's) are a repository-config concern and wait for the first
monorepo to need them.

**Defaults.** Workflow, per-task budget, per-initiative budget,
protected paths, supervisor model and per-lineage cap. Initiatives and
tasks inherit them; a task's own flags win. Configuration layering is
then: operator config, repository config, project defaults, task flags.

**Backlog.** A list of things worth doing that are not yet queued, each
one line or a paragraph, with a date. This is where "note that
somewhere" goes, and it is what an initiative is cut from. It lives in
the store, not in a file in the repository, because a project can span
repositories and because the backlog is not code.

**The record, scoped.** This is the part of a project that changes
behaviour rather than presentation. The supervisor, when it rules on a
task, reads that project's recent tasks and that project's decisions,
not the world's. Statistics, cost, defect escape and the human-rung rate
roll up per project. The journal stays per lineage and the repository
map per repository, as now.

**Why this is the tenancy unit.** In the operated model
(`docs/GTM.md`), a customer is a project: their repositories, their
budget, their secrets, their record, their intake. Projects now mean
that multi-tenancy later is a permissions question and not a data-model
change.

## Initiative

An initiative is the unit of operation: one outcome, pursued as a set
of tasks, tracked as one thing.

**Outcome.** One sentence saying what is true when the initiative is
done. It is placed in every task's prompt as the reason the task
exists, which is the context tasks lack today: a task that reads "make
the request path robust against upstream outages" above its own text
asks its question one paragraph sooner than one that does not.

**Tasks.** Created together, with their dependencies, in one command:
from a file of task texts, from the operator one by one with
`--initiative`, or by the planned workflow, whose investigate step
produces a plan whose items become the initiative's tasks. That last
path is the decomposition the turn-cap discussion called for, with a
home.

**State**, derived from the tasks: open while any is queued or running;
done when every task landed or was withdrawn; done with failures when
some failed and none remain; held when the worker is holding. Cost,
elapsed time, landed count and the human-rung count roll up.

**Stop rule and budget.** An initiative has its own budget and stops
claiming new tasks when it is spent, and stops when a configurable
number of its tasks fail in a row on the same rule, since three tasks
failing the same check say something about the initiative and not about
the tasks.

**One notification and one report.** When the initiative settles, one
event and one report: the outcome, each task and how it ended, what
verification refused, what the supervisor ruled, what reached a human,
what it cost. The report is generated from the record by the view
layer. No model writes it.

**Measurements are initiatives.** A comparison such as the journal
pairs is an initiative with a design; the report is the result.

## What is deliberately absent

No agent above a task. A project has no manager, an initiative has no
planner that runs on its own, and nothing prioritises the backlog for
you. The only thing above a task that thinks is the supervisor, which
now reads a project's record instead of the world's, and above it is
the operator. Forge 1 grew a director, a product manager and a learning
loop above its tasks, and the layers ate the product. A rule above a
task earns its place the way every rule does here, by rejecting
something.

No goals above projects. If a day comes with twenty projects and no way
to see which serve which purpose, that day will be obvious, and the
layer can be added then.

## Verbs

```
forge project new <name> --purpose <text> [--repo <path>[:<scope>]]...
forge project list | show <name> | backlog <name> [--add <text>]
forge project set <name> --purpose <text> --workflow … --per-task-usd … --supervisor-model …

forge initiative new <project> --outcome <text> [--from <file>] [--provider <name>] [--workflow <name>] [--budget <usd>]
forge initiative set <id> [--budget <usd>] [--stop-after <n>] [--outcome <text>]
forge initiative list [<project>] | show <id> | report <id> [--json]
forge add <repo> <text> --initiative <id>        (project follows the initiative)
forge add <repo> <text> --project <name>         (a task outside any initiative)
forge log | stats [--project <name>] [--initiative <id>]
```

Every shape joins the client contract (`docs/CLIENT.md`) so the TUI and
the web show projects, initiatives and their reports without shelling
out to anything else. The web gains a projects page, an initiative page
with its tasks and report, and the project filter on every list.

## Migration

Every existing task gets a project, assigned by repository: `forge`,
`nucleosynthesis`, `equitizr`. Tasks created together in the past stay
ungrouped; nothing is invented after the fact. From the migration on, a
task created without a project or initiative goes to the repository's
default project, and the default project for a repository is whichever
project lists it alone, or `forge add` asks.

## Build order

1. Schema and migration: projects, initiatives, the two columns on
   tasks, the backlog table, the default-project assignment.
2. Project verbs and defaults, with the configuration layering.
3. Initiative verbs: creation from a file, the rollup, the state, the
   stop rule, the settlement event, the generated report.
4. The record scoped: supervisor inputs, stats, defect escape, log
   filters.
5. Clients: the contract shapes, the web pages, the TUI.
6. The planned workflow filing a plan into an initiative.

The first thing to use it on is the next initiative.
