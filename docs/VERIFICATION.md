# Verification

Constitution 4: claims are verified, not believed. This document says exactly what
each verification level checks, who runs it, and how the result decides a Target's
terminal state. Stats report *verified* success separately from self-reported success
and never conflate the two.

## Levels

| Level | Question it answers | Run by | Decides |
|---|---|---|---|
| **L0** | Is the structured result internally consistent with Git? | worker, in the `verify` phase | `failed:result_unparseable` / `unverified` / pass |
| **L1** | Do the repository's declared checks pass when *Forge* runs them, and do they match what the agent claimed? | worker, in the `verify` phase | `unverified` / pass |
| **L2** | Does the thing actually work when an independent session exercises it? | a `verify`-mode attempt, new session, no shared context | `succeeded` / `unverified` |
| **L3** | Does a human sign off? | human, from the human queue | `succeeded` / `unverified` |

Each mode declares its required level (`MODES.md`). Verification is cumulative: L2
requires L0 and L1 to have passed first. A Target ends `succeeded` only if every
level up to the required one passed; it ends `unverified` if any level failed or could
not be run; it ends `failed` only for execution failures (non-zero exit, timeout,
cancellation, unparseable result). "Unverified" therefore always means *the agent
said it worked and Forge could not confirm it* — the state reflection should care
about most.

`VerificationCheck` is an interface (`Name()`, `Level()`, `Run(ctx, Subject) (Result,
error)`); the checks below are registered implementations, and a new check is one
file plus one `Register`. Each run writes one `verifications` row `{attempt_id =
subject, level, passed, verifier_attempt_id, verdict JSON}`; the highest level
attempted and whether it passed are summarised on `attempts.verification_level/_passed`
and copied to the facts. A failed level sets `unverified_reason` (`DESIGN.md` §4.1).

## L0 — consistency

Inputs: the parsed structured result, the mode's `Writes()` scope, and the
`git_inspect` outcome.

Checks, all of which must hold:

1. The result parses against the mode's schema and `schema_version` is one Forge
   knows. (Failure here is `failed:result_unparseable`, not `unverified`, because
   nothing can be verified.)
2. `changes[]` ⊆ changed paths in Git and changed paths ⊆ `changes[]`, where "changed
   paths in Git" is `git diff --name-status <base>..HEAD` ∪ the porcelain status
   entries of the worktree (committed and uncommitted alike; paths compared after
   cleaning; renames appear as delete + add).
3. Write scope:
   - `None` → worktree clean, `git_commits == 0`, `changes[]` empty.
   - `KbOnly` → same as `None` for the worktree (kb writes happen through the tool).
   - `DocsOnly` → every changed path matches the repo's `[modes.docs].paths`.
   - `Repo`, `NewProject` → no path restriction.
4. `needs_input` is null (a non-null `needs_input` never reaches L0: it is handled
   before, as a Question or as `ambiguity_at_auto`).
5. Every `claims[]` entry has non-empty `evidence`.

Verdict JSON records each check with pass/fail and the offending paths.

## L1 — declared checks

Inputs: the repository's `forge.toml` `[checks]` (as read from the worktree at
attempt start, so an agent editing `forge.toml` mid-attempt cannot change what it is
verified against) and the agent's `checks_run[]`.

Forge runs every declared check itself, in the worktree, after the agent has exited,
with the `forge_check` implementation (`internal/verify/check.go`, the same code the
MCP tool calls): each command runs with `Setpgid`, a per-check timeout (default
10 min, `[checks.timeouts]` overrides), bounded capture (last 64 KiB), and returns
`{check, passed, duration_us, failing_tests[], output_tail}`. `failing_tests[]` is
extracted by registered output parsers (`go test -json`, Jest, pytest, generic
"FAIL" lines); unknown formats yield an empty list, never a guess.

Rules:

1. Every declared check passes when Forge runs it.
2. For every check the agent listed in `checks_run[]` with `passed: true`, Forge's
   run also passed. A claim of `passed: true` that Forge cannot reproduce is the
   canonical **false claim** and yields `unverified` with reason
   `check_claim_mismatch:<name>` (smoke 20 simulates this by editing a fixture).
3. A check the agent did not claim but Forge runs is fine; the mismatch rule is
   one-directional (the agent may be conservative, never optimistic).
4. A repository with no `[checks]` passes L1 vacuously with `verdict.vacuous = true`;
   stats count these separately (`verified_vacuous`).

The declared checks are Forge-computed facts about the attempt; the agent's
`checks_run[]` is a claim. Both are stored; only Forge's decides.

## L2 — behavioural

Inputs: the subject attempt's structured result and `claims[]`, its head commit, and
the repository.

Flow:

1. On `complete` with L0 and L1 passed and the mode requiring L2, the control plane
   creates a Work (`trigger: dependency`, mode `verify`, routine snapshot derived
   from the subject's routine, `blocked_by` nothing) targeting the same repository.
   The subject Target stays `verifying`.
2. The worker prepares the verify attempt like any other, except `resolve_base`
   resolves to the **subject's head commit** (the worktree is a fresh checkout of the
   agent's branch at that commit; the subject's worktree is not shared). The prompt
   renders the claims and the mode's instructions (`MODES.md` §verify); the
   transcript of the subject is never included — no shared context.
3. The verify agent builds and runs the thing, hits endpoints, and for UIs drives a
   browser with Playwright from `<home>/deps` (installed by `forge init
   --with-browser`; the worker advertises `browser: ready` only when it is present,
   and UI verification routes only to such workers; Node is a test-time dependency
   only). Screenshots and logs are written to
   `{{artifacts}}` = `<data_dir>/artifacts/<verify-attempt-id>/`; Forge records each
   file as an `artifacts` row with size and SHA-256 after the attempt exits.
4. The verify result's `verdict` decides the subject: `pass` → `succeeded`; `fail` or
   `inconclusive` → `unverified` with the verdict attached. If the verify attempt
   itself fails (timeout, error) the subject is `unverified` with reason
   `verify_attempt_failed`.
5. The verify attempt's own L0 asserts it made no writes to the worktree.

The verify Work has no routine (`routine_id NULL`, so it is outside the subject
routine's `concurrency` — otherwise a `concurrency = 1` routine could never be
verified), is `interactive` if the subject was and `normal` otherwise, and carries the
subject's priority so verification does not starve behind backlog. It is created by
the subject mode's `FollowUps` (`MODES.md`).

## L3 — human

A Target whose mode or routine requires L3 (`routines.verification = "L3"` raises the
mode's level; it can never lower it) waits in `verifying` after L0–L2 pass, with an
item in the human queue: the summary, the diff stat, the L1 output tails, the L2
verdict and artifacts. `forge task approve <task>` (`POST
/api/v1/targets/{id}/approve`) → `succeeded`; `forge task reject <task> "reason"` →
`unverified` with `unverified_reason = human_rejected`. Waiting time is
recorded as `wait_human_us`.

## What gets recorded

- `verifications` rows per level with the full verdict.
- `attempts.verification_level` = highest level *attempted*, `verification_passed` =
  whether the required level passed.
- Facts copy both; stats derive `verified_success_rate = succeeded ∧ verification_passed
  / runs` and `self_reported_success_rate = ¬is_error / runs`, shown side by side.
- Every retro data pack includes, for each `unverified` attempt, which level failed
  and why, so reflection can target prompts that produce false claims.

## Forge's own UI

The Forge UI has Playwright tests (`ui/tests/*.spec.js`, run by `just ui-test` inside
`just check`) that start `forge daemon start --foreground` with `FORGE_HOME` set to a
temporary directory seeded with
fixture data, click through every page, and assert on content. They are the L2
mechanism for Forge itself and the fixture for smoke 22–23. Node and `npm` are needed
only to run them; the Forge binary builds without Node.
