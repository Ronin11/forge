# forge (2)

Proof of concept for the Forge rebuild, in Rust. One binary, one command
that matters:

```sh
forge run <repo> "<task>"   # worktree → agent → declared checks → fact row
forge log                   # attempts, newest first
forge show <id>             # one attempt in full
```

`run` does exactly this:

1. Reads `forge.toml` in the repo (`[checks]` table of argv arrays, optional
   `[defaults] base_branch`).
2. `git worktree add -b forge/<id> <FORGE2_HOME>/worktrees/<id> <base>`. The
   registered checkout is never touched beyond that.
3. Spawns `claude --print --output-format stream-json` in the worktree with
   a filtered environment, the task on stdin, and streams tool names to
   stderr. The raw stream-json is the log at `<FORGE2_HOME>/logs/<id>.jsonl`.
4. Counts commits and changed files with git, re-runs every declared check,
   and writes one row to `<FORGE2_HOME>/forge.db` (SQLite, table `attempts`).
5. Prints the terminal state: `succeeded`, `checks_failed`, `agent_failed`,
   or `unverified` (no checks declared). Exit code 1 for anything but
   success. The worktree and branch are left in place with the removal
   command printed.

Every number in the row comes from git, the CLI's own accounting, or Forge's
clock. Nothing the model says is recorded as a fact except its final text.

## Environment

- `FORGE2_HOME` (default `$XDG_DATA_HOME/forge2` or `~/.local/share/forge2`).
  Kept separate from Forge 1's `FORGE_HOME`.
- `FORGE2_CLAUDE_BIN` overrides the agent binary; any program that accepts
  the same flags and emits stream-json works, which is how the fake-agent
  smoke test runs.

## Deliberately absent

No daemon, no sockets, no auth, no sandbox (the agent runs with
`--dangerously-skip-permissions` in the worktree, on the host), no queue, no
retries, no UI, no personas, no learning loop. Each of those is added only
when a run demonstrates the need.

```sh
cargo build
cargo run -- run ~/some/repo "add a --verbose flag"
```
