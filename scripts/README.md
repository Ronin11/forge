# scripts/

Gate and smoke tooling for THIS repository only — what `just check` and
`just smoke` invoke (eval-check, bench-check, the milestone smoke suites,
the offline-gate proof).

Operator one-offs do not live here. Orchestration belongs in the system
itself — `forge task add`, workflows, bench specs (bench/specs/), library
scripts, forge_scratch — and a one-off that served its purpose lives on in
git history, not the working tree. (The greenfield stress-run submitters,
the overnight queues, and the routine-setup scripts all ended that way:
their durable content became bench specs and base-library seeds.)
