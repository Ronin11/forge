# Forge project review — 2026-09-22

Reviewed checkout: `ce29f81`. This is an implementation and architecture review, not an audit of the running production service, customer data, or historical outcome claims. No project source was changed.

## Judgment

Forge has a coherent and valuable core: a measured, independently checked path from a coding task to a deployed change. Its handling of failed attempts, integration, operator decisions, and provider limits is substantially developed. The largest problems are incomplete trust boundaries and interrupted-work semantics, rather than missing features. I would continue using it as a closely operated factory while prioritizing the findings below before expanding customer access or relying on hostile-code isolation.

## Validation

- `cargo test --workspace --offline`: 826 passed, zero failed, one ignored. The ignored test captures fixtures. The initial restricted run failed to bind local sockets; the approved unrestricted rerun passed.
- `cargo fmt --all --check`: passed.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: passed.
- Two isolated local reproductions demonstrated protected-file mutation during verification and execution of an agent-installed Git hook by the kernel's push.
- Those reproductions used `FORGE_SANDBOX=0`, fake agents, fresh temporary homes, and local bare remotes under `/tmp`. They prove the verifier and host Git behaviors; they are not an executed bubblewrap escape demonstration.
- Logs: `/tmp/forge-review-tests.log`, `/tmp/forge-review-fmt.log`, `/tmp/forge-review-clippy.log`.
- Reproductions: `/tmp/forge-review-repro.py`, `/tmp/forge-review-hook-repro.py`. Each invocation creates a new isolated fixture.

## The good

1. **There is one authority for executing work.** Clients use the CLI through `forge-client`; the boundary tests enforce important dependency restrictions. This reduces the risk of three interfaces developing different rules for landing, answering, or retrying work. See `client/src/lib.rs`, `tests/boundary.rs`.
2. **Verification and integration are real mechanisms.** Configuration is read from the trusted base; checks have bounded execution and output; hidden suites are overlaid; landing serializes per repository, fetches the current base, verifies the merge, and uses unforced pushes. These are the right architectural choices, notwithstanding the holes below. See `src/config.rs:281`, `src/checks.rs:140`, `src/landing.rs:196`.
3. **Failure behavior gets serious testing.** The tests exercise the real binary with temporary repositories and fake agents, including timeouts, rate windows, signals, retries, conflicting merges, and provider differences. The passing suite is meaningful, even though it lacks several adversarial invariants.
4. **The record is more useful than a success flag.** Attempts, verdict rows, decisions, cost, deployments, and lineage support diagnosis and measurement. The explicit distinction between a task fault and an environment fault avoids blaming an entire queue for a broken machine.
5. **The product has an operational point of view.** Budgets, questions, deployment checks, rollback, and customer communication are part of the system. The roadmap's measured unattended week is a better release criterion than accumulating features. Historical operating numbers in the docs were not independently verified in this review.

## The bad

1. **The project has several products' worth of responsibility.** The coding factory now also owns automation execution, triggers, customer intake, a portal, plugins, deployment, provider routing, and experiments. The review counted 6,242 lines in `cli.rs`, 4,231 in `view.rs`, 3,473 in `workflows.rs`, and 2,707 in `job.rs`; these totals include embedded tests. File length is a symptom, not itself a defect. The concrete concern is that guarantees implemented for tasks are not consistently implemented for jobs or portal calls. Extract application services around these boundaries before adding more branches to the CLI.
2. **The client boundary is sound, but its protocol is permissive.** Client structures default every field, and the common command runner waits for process completion without a timeout while buffering stdout/stderr. This tolerates additive changes but can hide missing required fields and strand a request on a stuck child. Introduce an explicit protocol version, mandatory identity/state fields, and deadlines with bounded output. See `client/src/lib.rs:1`, `client/src/lib.rs:69`.
3. **The event feed is not a durable transactional record.** Event file open/write/rename failures are ignored, and appending is separate from SQLite state changes. A state transition can succeed without the notification that a client or event-triggered workflow expects. Use a database outbox committed with the transition and export the stream from it. See `src/report.rs:345`, `src/store/events.rs:1`.
4. **Portal request handling has avoidable resource risks.** Bodies are read without a byte limit, one thread is created per request, and the subprocess client has no deadline. The token write limiter does not bound read traffic, body bytes, or concurrent child processes. See `portal/src/main.rs:242`, `portal/src/main.rs:811`, `client/src/lib.rs:69`.
5. **Documentation overstates or trails behavior.** README calls the web client read-only despite its mutation routes. It says reported changes must match Git, while `common_l0` now replaces the report with Git-derived changes. These are behavior and trust claims, not harmless wording drift. See `src/verify.rs:446`, `web/src/main.rs:1277`. The development checks live in `forge.toml`, but there is no checked-in CI workflow; `scripts/release.sh` refers to a future one. The factory benefits from an independent clean-environment validation path.
6. **The business proof is narrower than the feature set.** Building Forge and a few operated products demonstrates engineering utility. It does not yet establish customer support burden, external-integration reliability, or retained revenue. The ROADMAP outcome section remains unwritten. Finish the measured operating week and one tightly scoped customer automation before expanding features or treating agent spend as total delivery cost.

## The ugly

### 1. Critical: untrusted Git metadata reaches privileged host Git

The agent can write its clone's `.git`, including hooks and config. The Git wrapper directly invokes the host `git` binary against that clone, without disabling hooks or isolating repository-local executable configuration. Both task-branch and base pushes use it. A `.git/hooks/pre-push` created by the agent therefore runs as the operator during Forge's push, beyond the attempt's intended isolation boundary.

Sources: `src/sandbox.rs:265`, `src/git.rs:56`, `src/git.rs:274`, `src/git.rs:635`.

Reproduction: the fake agent installed a pre-push hook that appended a harmless marker under the temporary Forge home. Forge reported success and the marker recorded two hook invocations. Output is in `/tmp/forge-review-proof-5auildq7/output.log`. This reproduction was unsandboxed; the security conclusion additionally rests on the explicit host execution and writable-metadata paths in the code.

Fix: avoid host Git operations in an agent-controlled repository. Transfer vetted objects into a kernel-owned repository and operate there under controlled configuration. Disabling hooks is an immediate containment measure, but the review should also cover executable config, filters, helpers, and other Git subprocess entry points. Add an adversarial sandbox test whose only allowed outcome is that no marker appears outside the worktree.

### 2. High: verification can land changes made after L0 approval

L0 checks the clean tree and protected files before running repository checks. L1 executes commands in the writable clone. There is no invariant that HEAD and tracked content remain the same through the check run. Integration checks cleanliness but does not repeat the protected-file scope checks. A check can therefore create an additional commit after the protected-file gate.

Sources: `src/verify.rs:552`, `src/verify.rs:645`, `src/verify.rs:820`.

Reproduction: trusted base configuration named `bash check.sh`. The fake agent initially committed only a change to `check.sh`. During L1, that script modified and committed `forge.toml`. The trace showed `forge.toml-untouched` passing, and the local remote's main branch received the protected-file change. Output: `/tmp/forge-review-proof-8coh4w6e/output.log`.

Fix: bind the verdict to an exact candidate commit; reject unexpected HEAD/index/tracked-content changes across checks; recheck protected scope at integration; push the approved object ID. Allow build artifacts explicitly without permitting checks to rewrite the candidate. Also protect test-discovery and runner configuration where the assurance claim requires it: trusting an argv list alone does not make the referenced scripts trustworthy.

### 3. High: a portal token is not enforced as a project boundary on answers

The portal resolves a token to a project, then accepts an arbitrary posted task ID and calls `forge answer ID TEXT --by customer`. The resolved project is used to render the response, not to authorize the mutation. The kernel answer path checks that a task is answerable but has no project or token argument. A holder of project A's token can target an answerable task from project B by ID.

Sources: `portal/src/main.rs:656`, `portal/src/main.rs:730`, `src/cli.rs:1688`, `src/queue.rs:1022`.

Status: confirmed by tracing the implementation; no request was sent to a live customer service.

Fix: enforce expected-project and allowed-recipient constraints in the kernel mutation itself. Test two projects, two tokens, and cross-project answer requests. The existing fake-server test verifies argument forwarding and token validity, not cross-project ownership.

### 4. High: a crashed worker can strand jobs in running

Startup recovery finds orphaned tasks using worker PIDs. Jobs have no equivalent owner/lease field; claiming sets `state='running'`, and startup does not reclaim or reconcile running jobs. `requeue_job` is used on the explicit second-signal abort path. A crash or SIGKILL bypasses that path, leaving a running job outside the queued/scheduled claim query.

Sources: `src/worker.rs:780`, `src/worker.rs:958`, `src/store/jobs.rs:494`, `src/store/jobs.rs:580`.

Status: confirmed control-flow/schema gap; not exercised against the live worker.

Fix: persist job ownership/leases and recover interrupted work. Because jobs can send messages or perform other external effects, recovery also needs per-effect idempotency or explicit reconciliation; blindly rerunning a partially completed job is not sufficient.

### Additional isolation concern

Even after fixing host Git, the sandbox mounts shared writable Claude/Codex state and package caches. This is not strong isolation between unrelated customers: attempts can read or modify shared credential/session/config state, and mutable caches need a stronger trust argument than merely having lockfiles. Give attempts private provider state and separate agent credentials from the environment used for repository checks. Scope caches per trust domain or expose immutable validated cache content. See `src/sandbox.rs:142`, `src/sandbox.rs:266`, `src/config.rs:898`. No credentials were read during this review.

## Recommended sequence

1. Close host Git execution and portal cross-project mutation paths.
2. Make verification approve immutable commits and repeat scope enforcement at integration.
3. Add durable job recovery with safe external-effect semantics.
4. Isolate provider state; bound portal requests and CLI children; make events durable.
5. Add independent CI and repair behavioral documentation.
6. Run the measured unattended week and a narrow real customer workflow before broadening scope.

The core architecture is worth keeping. A rewrite would discard substantial tested operational knowledge. The next investment should make the existing guarantees true across every entry point and failure path.
