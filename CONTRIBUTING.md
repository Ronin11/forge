# Contributing

Forge lands changes on itself, so most changes are filed as tasks. Describe the
problem and observable acceptance criteria, then queue it with
`forge add <repo> "<task>" --workflow <workflow>`. See
[the quickstart](README.md#quickstart) and [workflows](docs/WORKFLOWS.md).
Size a kernel task to **one directive, table, or view**; split larger work into
ordered tasks with explicit dependencies.

## Build and check

Use Linux with Rust (edition 2024 support), Cargo, git, and **bubblewrap**
(`bwrap` on `PATH`). The integration suite uses fake agents inside the real
sandbox; model credentials are not needed for those tests.

Run the repository's declared checks from the repository root:

```sh
cargo build --workspace --all-targets
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

For release binaries, run `cargo build --release --workspace`. Bubblewrap is
required for normal sandbox coverage. On a box without it, the requested
`FORGE2_TEST_NO_SANDBOX` opt-out can be mapped to the current harness's renamed
variable, `FORGE_TEST_NO_SANDBOX`:

```sh
export FORGE2_TEST_NO_SANDBOX=1
FORGE_TEST_NO_SANDBOX="$FORGE2_TEST_NO_SANDBOX" cargo test --workspace
```

`FORGE2_TEST_NO_SANDBOX` alone is not read by the current harness. This fallback
runs attempts unsandboxed and does not prove sandbox isolation. Egress tests
also require permission to create network namespaces and may skip in a nested
sandbox. See [security posture](SECURITY.md).

## CI

`.github/workflows/ci.yml` runs on every push and pull request, on
`ubuntu-latest` with bubblewrap installed (`apt-get install bubblewrap`) so
the sandboxed e2e tests run for real. It runs the same three commands the
commit gate above runs — `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, and `cargo test --workspace` — caching the
cargo registry and `target` directory between runs. No fake agent or e2e
support script launches the real `claude` CLI; every test that exercises the
agent path points `FORGE_CLAUDE_BIN` (or `PATH`) at a fake under
`tests/fakes/`, so CI needs no model credentials.

## What a change must preserve

- **Workflows are mandatory.** Every task runs a workflow; verification and
  integration belong to the kernel and cannot be bypassed by an agent step.
  See [the workflow rule](docs/WORKFLOWS.md#the-rule).
- **A rule earns its way in by rejecting something.** Add a concrete negative
  case showing the invalid behavior it rejects, alongside valid behavior it
  accepts. A check that only passes does not demonstrate a useful boundary.
- **Checks come from the trusted base.** An attempt cannot weaken its own
  acceptance criteria by editing `forge.toml`. Respect protected paths and
  leave the verification namespace to verification. See
  [checks](docs/CHECKS.md) and [verification](src/verify.rs).
- **Keep the client boundary.** TUI, web, and portal clients use the CLI through
  `forge-client`, its JSON documents, and its event stream. They must not open
  the database or link the kernel. Extend [the client contract](docs/CLIENT.md)
  when extending that interface; [the boundary test](tests/boundary.rs) enforces
  the separation.
- **Gate commits on the suite's exit status.** For local development, run the
  suite and require a zero exit status before committing; do not infer success
  from a partial log or lose the status through a pipe. In a Forge attempt,
  follow the attempt's commit protocol: Forge reruns the trusted suite after
  the agent exits and gates acceptance and landing on its result. Report only
  checks actually run and their real outcomes; an unfinished run is not a pass.

Keep changes focused, explain the behavior being changed, and provide concrete
validation evidence. The standing sizing rule is one directive, table, or view
per task; [the second engineering review](docs/REVIEW-2.md) records its context.
For vulnerabilities, follow [the private reporting guidance](SECURITY.md#reporting-a-problem).

## Source file size

Every tracked Rust file in the workspace is limited to 1,500 lines by
`tests/file_size.rs` (`git ls-files` excludes build output). Split larger files
by responsibility the way `src/store/` and `src/cli/` were split. The test's
allowlist records pre-existing large files, each with a reason and a ceiling
of its line count at the CLI split's base plus 200. Do not add exceptions or
raise ceilings. Delete an entry when its file is removed or reaches 1,500 lines
or fewer; stale entries fail the test. CLI functions also retain their 80-line
limit and existing exceptions in `src/cli/tests.rs`.

## Function length

Every function in a tracked Rust file, test-module functions included, is
limited to 120 lines (signature through closing brace) by `tests/fn_length.rs`.
Its allowlist records pre-existing long functions as (file, fn, ceiling,
reason), each ceiling being the function's length when the rule was added plus
20. A reason may say "test fixture", but the ceiling still holds. Do not add
exceptions or raise ceilings. Delete an entry when its function shrinks to 120
lines or fewer or is removed; stale entries fail the test, and an offender is
reported with its length and ceiling.
