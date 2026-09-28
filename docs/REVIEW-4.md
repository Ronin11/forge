# Fourth architectural review: the edges, and the weekly measurement (2026-09-27)

The kernel's edges are the places it meets something it does not own: the
sandbox and the egress proxy, plugin supervision and the successor
handoff, and deploy with the release layout. The first three reviews read
the inside (docs/REVIEW.md, REVIEW-2.md, REVIEW-3.md); this one reads the
boundary, cold, and records a defect only once it has been confirmed at
its line, with the input that reaches that line and what goes wrong
there. Suspicions are not recorded. Each defect ends with a paragraph a
follow-up task can be filed from as written, and each section ends with a
closing table the fix initiative is filed from.

Sections: 1, the sandbox and the egress proxy (this task); 2 and 3, the
other two edges, each a separate cold read written by its own task;
4, the weekly measurement's own finding (`engineering-weekly` fired the
same day on `src/workflows.rs` at 3671 lines), which was written first and
is kept whole.

## 1. The sandbox and the egress proxy

Read as one system: src/sandbox.rs (1005 lines), src/egress.rs (1238),
src/executor.rs (428), src/environment.rs (704), src/login.rs (411, the
credential write-back task 806 landed) and the launch paths of
src/agent.rs (3473: `agent_env`, `command_in`, `run_once`,
`run_with_relaunch`, `run_claude`, `run_codex`, `run_copilot`,
`run_json_phase`) with src/agent/refusal.rs (197). Read beside them when a
question depended on it: src/checks.rs `run_one_capped`, src/attempt.rs
(spec, launch, verification checkout, `record`), src/verify.rs (`l1_l2`,
`red_on_base`), src/ctx.rs (`allow_egress`, `apply_grant`,
`declare_cache`), src/env_supervisor.rs, src/engine.rs
`apply_environment`, src/git.rs, src/landing.rs and src/cli/gc.rs (where
provider state is discarded), src/worker.rs (`claim_egress_dir`, the
config reload) and src/config.rs (`[sandbox]`, `[execution]`, `[trust]`).

Read against the boundary as it is built, so a defect can be located on
it:

- **Enters an attempt.** `/usr`, `/etc` (whole), `/opt` and the lib
  directories read-only; a tmpfs `$HOME` with holes: the agent binaries'
  directories and `[sandbox] ro_paths` (default `~/.local/share/mise`) and
  the forge binary's directory, read-only (sandbox.rs:510-512); the
  operator's `~/.claude.json` copied in (513-515, 432-453); the worktree,
  `.git` included, read-write (529); a private provider directory per
  worktree, `<worktree>-provider`, seeded on every launch from the host's
  `.claude/.credentials.json` and `settings.json`, `.codex/auth.json` and
  `config.toml`, `.copilot/config.json` and bound read-write where each CLI
  looks (540-570); `[sandbox] rw_paths` (`~/.npm`, `~/.cargo/registry`,
  `~/.cargo/git`) as discarded overlays (579-587); the dependency cache
  read-only; environment grants read-only (592-598); the repository's
  `FORGE_CACHE_DIR` read-write (602-604); the proxy socket (526-528); and
  `agent_env`'s variables (agent.rs:286-311: `PATH`, `HOME`, `LANG`,
  `TERM`, `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `LC_*`, `ANTHROPIC_*`,
  `CODEX_*`, `COPILOT_*`) plus the provider's own.
- **Leaves an attempt.** The agent's stdout and stderr (agent.rs:571-586);
  the worktree, whose commits the kernel fetches (git.rs:455-475) and
  whose `.git` it then replaces by a kernel-made one
  (attempt.rs:304-312); a refreshed login, copied over the host file
  (login.rs:169-187); the relay's refusal record, `.git/forge-egress-
  refused.jsonl`, which becomes environment needs and grants
  (egress.rs:638-704, environment.rs:99-137); and the traffic the proxy
  allows (egress.rs:378-510).

Five claims below were reproduced by hand on this host (bubblewrap 0.12.0,
git 2.55.0, rustc's `std`) rather than argued: defect 1 (`std::fs::copy`
onto a symlink that names its own source), defect 3 (`git status
--porcelain` running `core.fsmonitor`), defect 17 (a SysV shared-memory
segment made outside is listed inside a sandbox built with forge's flags),
defect 20 (a read-only bind under a directory later bound over is gone)
and defect 24 (`DirBuilder::recursive` accepts an existing world-writable
directory and leaves its mode). Nothing else was run: the rest is read
from the lines cited, and where a defect rests on a CLI's behaviour (4, 8,
18, 25) it says so.

### 1.1 Defects confirmed while reading

1. **Seeding a private copy writes through a symlink the sandbox can
   plant, so an attempt can empty the operator's real login, or overwrite
   any file the operator can write.**
   `login::seed` ends with `std::fs::copy(&host, private)` (login.rs:232),
   where `private` is `<worktree>-provider/claude/.credentials.json`
   (sandbox.rs:548-552): a path inside a directory that is bound
   read-write into the sandbox (sandbox.rs:560), so the sandbox owns every
   entry in it. The settings, codex and copilot seeds are the same call on
   sandbox-writable destinations (sandbox.rs:553-559, 566-569), and so is
   `std::fs::write(&schema_path)` into the worktree's `.git`
   (agent.rs:1654). `fs::copy` opens its destination `O_TRUNC` and follows
   a symlink.
   *Input:* any process in the sandbox, the agent or a check (defect 19),
   runs `rm ~/.claude/.credentials.json; ln -s
   "$HOME/.claude/.credentials.json" ~/.claude/.credentials.json`.
   `$HOME` inside is the operator's real home path (sandbox.rs:612 forces
   `home`, which is the operator's own), so the link text names the real
   host file, which the sandbox cannot see and the host process can.
   *Reproduction:* the next launch in that worktree calls `seed`, which
   holds the lock, runs `write_back_locked` over every private copy (the
   read at login.rs:175 follows the link and reads the host file itself:
   same expiry, nothing to write), finds the host `Usable`, and calls
   `fs::copy(host, private)`: source and destination are one inode, the
   destination is truncated first, and zero bytes are copied. Reproduced
   with a twelve-line program: the copy returns `Ok(0)` and the source
   reads `""`. The host file is now an empty login; `host_state` says
   `Empty`, every later launch is refused ("the agent login … has an empty
   token", refusal.rs:56-68), and the operator's own `claude` is logged
   out. This is the 2026-09-27 outage, produced on demand by an untrusted
   task. Pointed at `~/.bashrc` or `~/.ssh/authorized_keys` instead, the
   same call overwrites it with the credentials JSON (or the settings or
   codex config).
   *Task:* No host-side write may go through a path the sandbox can
   write. Give login.rs one helper that writes bytes to a sibling temp
   file created `create_new` and renames it over the destination (the
   rename replaces a symlink, never follows it), reuse `replace_atomic`
   for it, and use it at every seed site: `login::seed`, the settings,
   codex and copilot copies in `Sandbox::command`, and the codex schema
   file in `run_codex`. Before any host-side read of a sandbox-writable
   file, require `symlink_metadata` to say regular file (defect 2 and 15
   add the size bound). Test: a unit test where `private` is a symlink to
   the host file, asserting `seed` leaves the host file byte-identical and
   leaves a regular file at `private`; the same with a symlink to an
   unrelated file (untouched); and one for the codex schema path.

2. **The write-back accepts any file the sandbox wrote as the operator's
   new login, so an attempt can replace it with junk or with another
   account's.**
   `should_write_back` (login.rs:109-115) is: the private pair has both
   tokens non-empty, expires later than the host's, and the host is usable
   or the private one unexpired. `write_back_locked` (login.rs:169-187)
   reads the whole file (`fs::read`, no size bound, link followed) and
   `replace_atomic`s it over the host's. Nothing ties the pair to the one
   that was seeded. sandbox.rs:46-52 still says an attempt "can neither
   read nor overwrite the operator's actual session state"; since task 806
   it can overwrite it.
   *Input:* any process in the sandbox writes
   `{"claudeAiOauth":{"accessToken":"x","refreshToken":"y","expiresAt":
   9999999999999}}` to `~/.claude/.credentials.json`.
   *Reproduction:* after the launch, `guarded_claude` calls
   `write_back_login` (refusal.rs:47, sandbox.rs:384-389), or a later
   `seed` from any task does: `private_copies` (login.rs:205-222) reads
   every `*-provider` sibling, including another task's and a public-trust
   task's. The host is usable, the private expiry is later: the operator's
   file is replaced. Consequences, in order of likelihood: the pair is
   junk, every launch seeds junk and the CLI fails to authenticate; because
   `expiresAt` is far ahead, `near_expiry` (login.rs:73-75) is never true,
   so the kernel never refreshes and never notices until someone runs
   `claude login`; or the pair is the attacker's own real one, and the
   operator's launches run on the attacker's account; or the file is
   gigabytes and `fs::read` holds it all in the worker.
   *Task:* Accept a private pair only if the CLI could have produced it
   from the seed. At seed time record, outside anything the sandbox can
   write (a file in FORGE_HOME keyed by the private path), a hash of the
   seeded refresh token and the seeded `subscriptionType` and scopes;
   accept a private pair only if its refresh token differs from the
   seed's (a rotation happened), `expiresAt` is at most a day ahead, the
   file is a regular file of at most 64 KiB, its token strings have the
   shapes the CLI writes, and its other fields equal the seed's. Keep the
   file it replaces as `.credentials.json.forge-prev` (0600, one copy) so
   an acceptance made in error can be undone by hand, and say so in the
   note the launch prints. Test: unit tests for each rejection (same
   refresh token, far expiry, oversize, symlink, changed scopes) and one
   accepted rotation; an e2e test where a fake agent writes a forged pair
   and the host file is unchanged afterwards.

3. **The kernel runs git on the host inside a directory the sandbox has
   just written, unhardened, so a sandboxed process can run code as the
   operator.**
   git.rs:1-9 and the test at git/tests.rs:118-122 hold that the one
   command in an agent's clone is a hardened fetch. The other calls that
   name a sandbox-written directory are plain `Git::new(dir)`: `head`
   (git.rs:754), `changed_paths` (779), `dirty_paths` (923),
   `dirty_tracked_paths` (933). `git status` honours `core.fsmonitor` from
   the repository's own config, and `.git/config` is inside the worktree
   bound read-write (sandbox.rs:529). The kernel replaces `.git` with a
   kernel-made one after the agent exits (attempt.rs:304-312), but the
   checks then run in the sandbox on that same directory (verify.rs:708,
   1086, 1101, with `.git` writable again), and `record` runs
   `git::dirty_paths(dir)` afterwards (attempt.rs:609). Three more sites
   run it straight after an agent with no checkout in between:
   agent.rs:1716 (the codex nudge), supervisor.rs:526 and 616, assess.rs:
   163 and 191.
   *Input:* a check, or an agent, runs `git config core.fsmonitor
   "$PWD/hook.sh"` (the worktree has the same path inside and outside).
   *Reproduction:* the kernel's next `dirty_paths` on that directory runs
   `hook.sh` as the operator with the real `$HOME`: reproduced with git
   2.55.0, `git status --porcelain` executed the hook. A
   `filter.<name>.clean` command on a tracked file is the other thing
   `status` runs from config (not reproduced). The ssh backend copies the remote
   tree back over the local one with `rsync -a --delete`
   (executor.rs:135-138), `.git` included, so it reaches the same call
   with the remote host's content.
   *Task:* Make no host git command trust a repository the sandbox could
   write. In git.rs, give `Git` a default hardening for every call that is
   not on a kernel-owned directory: `-c core.fsmonitor=false -c
   core.hooksPath=/dev/null -c core.attributesFile=/dev/null`, `--no-
   optional-locks`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`;
   and add `git::restore_metadata(dir)`, which rewrites `.git/config` from
   `KERNEL_CONFIG`, empties `hooks/` and `info/`, and is called after
   every sandboxed launch or check phase in a directory before the first
   host git command there (the agent.rs, supervisor.rs, assess.rs, verify
   and attempt sites above). Extend the call-site test in git/tests.rs so
   a new unhardened call on a non-kernel directory fails it. Test: a unit
   test that sets `core.fsmonitor` to a script in a temp repo and asserts
   `dirty_paths` does not run it, and an e2e test whose check plants the
   config and asserts no marker file appears.

4. **The login refresh probe runs the claude CLI on the host, unsandboxed,
   in the task's worktree, with the repository's own settings in force.**
   `probe` (refusal.rs:114-136) builds the attempt's own argv
   (`claude_argv`, agent.rs:841-880, `--setting-sources project,local`,
   so "the repository's own may apply") and launches it through
   `command_in(None, l.worktree, …)` (refusal.rs:120). With no execution
   passed, `command_in` uses `Host` (agent.rs:325-331), whose `command`
   sets `current_dir(worktree)` and the real environment, `HOME`
   included (executor.rs:63-76). `refresh_on_host` reaches it whenever the
   host login is within 30 minutes of expiry (refusal.rs:86-88), that is,
   about once per token lifetime, on the first claude launch after.
   *Input:* any task whose worktree carries a project-level claude
   setting (`.claude/settings.json` or `settings.local.json` in the
   repository, or written by an earlier attempt: the worktree persists
   across attempts). *Reproduction:* the probe starts the CLI in that
   directory, outside bubblewrap, on the host network, with the operator's
   real `~/.claude`, `~/.ssh` and environment. The sandbox exists because
   those settings can name commands (hooks) and the lean flags at
   agent.rs:841-855 were added because the CLI otherwise loads what the
   operator did not intend; here the same CLI, same settings sources, runs
   without it. This part is read, not run: that the CLI executes project
   hooks in `--print` mode is its documented behaviour, and the probe's
   only protection is `--tools ""`, which limits the model, not hooks.
   `kill_on_drop` after the 120-second wait (refusal.rs:134) kills the
   CLI, not anything it started.
   *Task:* Run the probe in a kernel-made empty directory
   (`FORGE_HOME/probe/`, recreated empty each time, no `.claude`, no
   `CLAUDE.md`, not a git repository), never in a task's worktree, and
   with `--setting-sources user` if the CLI accepts it there (else none of
   the project sources), keeping the real config directory so the refresh
   lands on the host file. Make its child group killable (`process_group`)
   and kill the group on timeout. Test: an e2e test with a worktree whose
   `.claude/settings.json` names a hook that writes a marker and a fake
   agent binary recording its cwd; assert the probe's cwd is not the
   worktree and no marker appears.

5. **A trust level that promises model-only egress runs with none of it
   when the backend is not bubblewrap, and nothing refuses.**
   `[trust.public] egress = "model"` is the default (config.rs:812) and
   `Forge::allow_egress` (ctx.rs:268-278) enforces it by telling the
   `Sandbox`. On the `Host` backend `Execution::command` computes the
   policy and drops it (executor.rs:235-251); `Ssh` never sees it;
   `FORGE_SANDBOX=0` makes `Execution::detect` return `None`
   (executor.rs:188) and every launch goes through `Host` (agent.rs:325).
   A machine without bwrap gets the host backend silently
   (`default_backend`, executor.rs:153-159). `Backend::guarantees` knows
   (`egress_bounded: false`, executor.rs:31-39) but only `forge doctor`
   reads it. `apply_trust_policy` (queue.rs:190) checks the budget, the
   workflow and protected paths, not the executor.
   *Input:* a `contact` or `public` task (a stranger's issue) filed
   against a repository that declares `[execution] backend = "host"` or
   `"ssh"`, or on a worker with `FORGE_SANDBOX=0`, or on a host that lost
   bwrap. *Reproduction:* the task is claimed and run; the agent and its
   checks run as the operator with the real home, `~/.ssh`, `~/.claude`
   and the full network. The attempt's recorded inputs say `executor:
   host` (ctx.rs:361-376); nothing stops it, and `auto_land` being false
   at that level is the only remaining brake.
   *Task:* Refuse to start a task whose trust level is not `operator`, or
   whose `[trust]` egress is `model`, unless the backend it would run on
   reports `egress_bounded && worktree_private`. At enqueue, fail with the
   backend and level named; at claim (the backend can change under a
   queued task), leave the task `blocked` with that reason, never
   `failed`. A `[trust.<level>] allow_unsandboxed = true` (default false)
   lets an operator opt a level out, and `forge doctor` flags the opt-out.
   Test: unit tests over the gate for each backend and level; an e2e test
   with `backend = "host"` and a `public` task that asserts the task is
   blocked with the reason and no agent was launched, and that an
   `operator` task on the same repository still runs.

6. **A missing host cache is granted read-only at any trust level, from a
   path named in text the attempt itself wrote.**
   `recognize` reads the failing checks' tails, the attempt's reason and
   the agent's own `needs_input` question (engine.rs:399-424) and
   `missing_cache` turns any line with "not found", "ENOENT" or similar
   and a `/.cache/` path into a `Cache` need (environment.rs:196-215).
   The doc on `grant_environment` (ctx.rs:280-284) says it returns `None`
   when "the trust level reaches the model endpoints alone", but
   `apply_grant` gates only host grants on that (ctx.rs:312); a
   `ReadOnly` grant is applied at every level (ctx.rs:316).
   `env_supervisor::applies` is `true` for every cache need whatever the
   level (env_supervisor.rs:88), and its ceiling is one directory under
   `~/.cache`, any directory (environment.rs:405-431).
   *Input:* a `public` task's check prints `ENOENT: no such file
   /home/u/.cache/huggingface/token`, or the agent asks a question
   containing that line. *Reproduction:* the need is typed `Cache`; the
   default table covers only `~/.cache/node-gyp` and
   `~/.cache/ms-playwright`, so, when the supervisor is enabled, it goes to
   the supervisor, an LLM reading the attacker's evidence line; a single approval within the ceiling
   binds `~/.cache/huggingface` read-only into every later attempt in the
   worktree (sandbox.rs:592-598). A stranger's task now reads the
   operator's Hugging Face token (or `~/.cache/<anything>`: package
   manager caches with private packages, browser caches) and can put it in
   a commit or its output. The policy path itself is safe: `covers`
   returns the table's own directory, not the need's (environment.rs:
   337-343).
   *Task:* Gate `Grant::ReadOnly` in `Forge::apply_grant` on the trust
   level exactly as `Grant::Host` is, and make `env_supervisor::applies`
   the same for caches. Replace the supervisor's "any directory under
   `~/.cache`" ceiling by the operator's own list: only paths under
   `[environment] cache_paths` (the table) may be granted, by policy or by
   the supervisor; a need outside it is a question to the operator, never
   an approval. Fix the doc comment to match. Test: unit tests that a
   `public` task's `ReadOnly` grant is `None` and that the ceiling refuses
   `~/.cache/huggingface` when it is not in the table; an e2e test that a
   check printing that line under `public` trust ends with no bind.

7. **The supervisor's ceiling lets it approve an address the operator
   would never write, and an exact rule is never checked against where the
   name resolves.**
   `valid_host` (environment.rs:186-191) accepts any dotted string,
   including `169.254.169.254` and `10.0.0.5`; `host_ceiling`
   (environment.rs:375-403) refuses wildcards, github.com, model
   endpoints and the repository's `deny`, and nothing else. The grant
   becomes `Rule::parse(host)`, an `Exact` rule (ctx.rs:314), and `dial`
   applies the loopback/private/link-local check to `Matched::Suffix`
   only (egress.rs:362): the design says an exact host is one "the
   operator wrote down" (egress.rs:51-57), which a granted one is not.
   *Input:* an operator-trust task whose install script prints `403
   http://169.254.169.254/latest/meta-data/…` (the by-URL branch,
   environment.rs:162-175), or names an internal hostname that resolves
   privately. *Reproduction:* the need is a `Host`; `host_ceiling` says
   ok; if the supervisor approves, the proxy now connects an attempt to
   the cloud metadata service or a LAN address; docs/ops carries a
   Hetzner cloud-init, and on that host the service serves the
   instance's user-data.
   *Task:* Make a granted host as suspect as a suffix match. Refuse IP
   literals, `localhost` and single-label names in `host_ceiling`; and add
   `Matched::Granted` (a rule built by `apply_grant`, not by the operator
   or a repository's forge.toml) that `dial` treats like `Suffix`: resolve,
   refuse non-public addresses, connect to the address it checked. Test:
   unit tests for the ceiling on those inputs and a `dial` test where a
   granted name resolves to 127.0.0.1 (refused with the resolved address
   named) while an operator-written one still connects.

8. **Codex's and copilot's logins are seeded and never written back, the
   shape of the 2026-09-27 outage that the claude fix did not reach.**
   sandbox.rs:557-559 copies `~/.codex/auth.json` and `config.toml`, and
   566-569 `~/.copilot/config.json`, into the private directory on every
   launch and nothing reads them back: `write_back_login` is the claude
   pair only (sandbox.rs:384-389), `login::FILE` is `.credentials.json`.
   *Input:* a codex or copilot launch whose CLI refreshes its token. That
   the codex CLI rotates its ChatGPT refresh token and refuses a reused
   one is the CLI's behaviour, not read in this repository; the task
   below starts by reproducing it. *Reproduction:* if it rotates, every
   sandbox that starts with a stale `last_refresh` refreshes the same
   seeded token at once, one wins, none is written back, and the host's
   `auth.json` holds a dead refresh token for the operator's own codex
   and for every later seed: the claude outage, per provider, with no
   doctor row (doctor/login.rs covers `.credentials.json` only).
   *Task:* Reproduce codex's refresh in a sandbox with a short-lived
   `auth.json` first; if the refresh token rotates, generalise `login` to
   a small per-runner description (file, how to read expiry and token
   presence, how to fingerprint the seed) and use it for codex and
   copilot with the same lock, seed, guarded write-back (defects 1, 2) and
   doctor row. If the CLI does not rotate, record that in login.rs's
   header and close this. Test: the login unit tests parameterised over
   the three shapes.

9. **The private pair is the only live one until it is copied back, and
   nothing makes the copy-back loud or certain before the copy is
   deleted.**
   `Sandbox::write_back_login` returns `false` for "nothing to do" and for
   "failed" alike (`unwrap_or(false)`, sandbox.rs:388); `seed` discards
   every write-back error (`let _ =`, login.rs:228). Three places delete a
   provider directory without a last write-back: `forge gc`
   (cli/gc.rs:45-49), and the landing's scratch trees
   (landing.rs:154-158, 1042, 1163). A launch that is aborted (the second
   signal, worker.rs:1097-1102) never reaches `guarded_claude`'s write-back
   at all (refusal.rs:46-52).
   *Input:* a claude attempt whose token refreshed, followed by a failed
   write-back (host file read-only, disk full, the config directory
   owned by another user) or an aborted launch, and then `forge gc` or a
   landing removing that worktree before any other claude launch anywhere
   runs `seed`. *Reproduction:* the private copy held the only live
   refresh token; the directory is removed; the host file's token is the
   dead one; the next launch fails to refresh and the CLI empties the file:
   the outage again. The window is short on a busy queue and open
   overnight on an idle one.
   *Task:* Make the write-back's outcome three-valued (`Done`, `Nothing`,
   `Failed(err)`) and print a `login` note on `Failed`; have
   `discard_provider_state` call `login::write_back` for the directory it
   is about to remove and refuse to remove it when that fails; and have
   the worker's start (`recover_orphans`) run a write-back over every
   private copy once. Test: unit tests over the three outcomes, and one
   where `write_back` fails (read-only host directory) and
   `discard_provider_state` leaves the copy in place.

10. **`flock` blocks a runtime worker thread, and the refresh probe holds
    the lock for up to two minutes across an await.**
    `login::lock` calls `libc::flock(…, LOCK_EX)` synchronously
    (login.rs:150-160). It is reached from `Sandbox::command` through
    `seed` (sandbox.rs:548), which every launch, including every check
    phase (checks.rs:177) and landing verification, calls on a tokio
    worker thread, and from `write_back_login` (refusal.rs:47).
    `refresh_on_host` takes the same lock and holds it while it awaits the
    probe's child and a 120-second timeout (refusal.rs:76-93, 134).
    *Input:* the host login is near expiry, one slot is refreshing, and
    other slots start check phases. *Reproduction:* each of those
    launches blocks its worker thread in `flock` until the probe ends.
    The runtime has as many worker threads as cores (`#[tokio::main]`,
    main.rs:71); when blocked launches equal the thread count (four slots
    on a two-core host: the refreshing slot plus three blocked), nothing is
    left to poll the probe's child or drive its timeout timer, the probe
    never finishes, the lock is never released: a deadlock that only
    killing the worker ends. Short of that, every slot's launch stalls
    behind the refresh.
    *Task:* Never block a worker thread on the login lock. Split seeding
    out of `Sandbox::command` into an async `prepare(worktree)` that the
    callers (`run_claude`, `run_json_phase`, `checks::run_one_capped`) await
    before they build the command, taking the lock with `spawn_blocking`
    or a `try_lock` loop with `tokio::time::sleep`; and let
    `refresh_on_host` hold the lock only around the compare and the write,
    running the probe under a second lock that `seed` waits on
    asynchronously. Test: a runtime with one worker thread, the lock held
    by another task, and a second task asserted to make progress.

11. **A launch whose route out is broken proceeds, and the run is judged
    as if the agent had failed.**
    `check_socket` (sandbox.rs:411-422) treats any error from `socket_for`
    as "no runtime means no route, which `command` already tolerates" and
    returns `Ok`, but `socket_for` also fails when the proxy directory
    cannot be created or the socket cannot be bound (egress.rs:742-760),
    which is exactly what a directory that vanished under a running worker
    produces (tasks 723 and 755). `command` then prints one line to stderr
    and builds the launch with `--unshare-net` and no relay
    (sandbox.rs:519-525, 613). And `check_socket` is called from one place,
    `agent::run` (agent.rs:795): checks (checks.rs:177), operations,
    the login probe and job steps go straight to `command_in`.
    *Input:* the proxy directory or a socket in it removed (a tmp cleaner,
    an operator, a bug like 723), then any launch that is not an agent run
    for an existing policy (bwrap fails "Can't find source path", and the
    check fails with that as its tail), or any new policy (the bind fails,
    the launch has no network). *Reproduction:* the check's tail is
    `bwrap: Can't find source path /tmp/forge-egress-…/p0.sock`, fed back
    to the coder as a failed check; or the agent starts with no route,
    fails at its first request, and is counted as a failed attempt.
    Neither is an environment fault, and neither retries the launch.
    *Task:* Make `socket_for` return a typed error (`NoRuntime` versus
    `Failed`), have it verify that the socket it returns still exists and
    rebuild the directory and the listener when it does not (the proxy is
    process state, so recreating it is safe), and move the socket check
    into `Execution::command` so every launch path has it: a broken route
    is `Fault::Env`, never a check result. Test: remove the proxy
    directory under a running `Proxies` and assert a check launched
    afterwards runs (healed), and that an unbindable directory yields an
    env fault naming the path rather than an offline launch.

12. **The egress relay's start is unchecked: a relay that never came up
    means an offline agent after a five-second wait, and the relay path
    can be a name that no longer exists.**
    The wrapper (sandbox.rs:424-455) starts `forge egress-relay … >/dev/null
    2>&1 &`, waits up to 500 × 10 ms for the ready file, and then
    `exec "$@"` whether or not it appeared (441-449). The relay's stderr
    is discarded, so a failure to exec or to bind leaves no trace.
    `relay_exe` is `std::env::current_exe()` (sandbox.rs:289), which Linux
    reports as `…/forge (deleted)` after a rename-over replacement;
    binary.rs has a helper for it, plugins and the unit use
    `binary::launch_path()`, and docs/OPS.md:22-32 records a "(deleted)" path as
    the cause of one of three incidents. The sandbox reads
    `current_exe()` raw, at every `Sandbox::detect`, and `detect` runs
    again on every config reload (`Forge::reopen`, ctx.rs:233;
    worker.rs:938), and again at ctx.rs:194 and agent.rs:275 for the
    directory to bind.
    *Input:* `forge upgrade`'s copy-then-rename install (docs/OPS.md step
    7) over the running binary, then a config edit or SIGHUP before the
    worker restarts. *Reproduction:* the reloaded `Forge` carries a
    relay path that does not exist; every launch it makes waits five
    seconds, starts the agent with `HTTP_PROXY` naming a port nothing
    listens on, and the agent fails offline, as an agent failure, with
    nothing in any log about why.
    *Task:* Resolve the relay and bind directory through
    `binary::launch_path()` (or `without_deleted_suffix`) at the three
    sites; make the wrapper print `forge: the egress relay did not start`
    with the relay's own stderr (send it to a file in `/run/forge` and
    `cat` it) and `exit 125` when the ready file does not appear, and make
    the engine classify exit 125 with that text as `Env`. Test: a
    launch with a relay path that does not exist exits 125 within about
    five seconds with the message, and a unit test that `Sandbox::detect`
    strips a ` (deleted)` suffix.

13. **The retry for bwrap's transient bind-mount failure covers the claude
    agent and nothing else.**
    `is_transient_bwrap_failure` and `run_with_relaunch` (agent.rs:516-528,
    777-779) are called from `run_claude` alone (agent.rs:912). Codex and
    copilot go through `run_json_phase` (agent.rs:1509-1533), checks
    through `run_one_capped` (checks.rs:177-215), the probe and operations
    through their own spawns, none of which retries. Every one of them
    binds the operator's `~/.claude.json` at the seed path
    (sandbox.rs:513-515), the file the operator's own interactive `claude`
    rewrites every turn, which is the race the comment at agent.rs:516-519
    describes.
    *Input:* a check phase or a codex/copilot launch that starts while the
    operator is using `claude` on the host. *Reproduction:* bwrap exits
    "Can't bind mount …", in under two seconds; a check reports it as its
    tail and fails, and the coder is told its checks failed; a codex
    launch is an agent failure that spends an attempt.
    *Task:* Move the transient-failure detection and the relaunch loop out
    of `run_claude` into one helper beside `command_in` that every
    sandboxed spawn uses (agent phases, checks, operations, probe), and
    keep the note it prints. Test: a fake `bwrap` that fails twice with
    that message and then runs, driven through `run_one_capped` and
    `run_json_phase`.

14. **The proxy has no bound on connections, on idle tunnels or on what it
    logs, and its accept loop spins on error.**
    `serve` (egress.rs:274-284) accepts in a loop and `continue`s on any
    error with no pause and no limit; every accepted connection is a task;
    a tunnel (`copy_bidirectional`, egress.rs:440-442, 506-508) has no idle
    or total timeout; `refuse` writes one stderr line per request with the
    request's host verbatim (egress.rs:310). All attempts' proxies live in
    the worker process, which has the worker's descriptor limit, and the
    sandbox reaches the socket directly (`/run/forge/egress.sock`), not
    only through the relay.
    *Input:* one attempt opens connections to an allowed host (any
    `*.anthropic.com` name) from several processes and holds them.
    *Reproduction:* each connection costs the worker two descriptors and
    the relay's per-process limit does not bind several processes; at the
    worker's limit (typically 1,024 soft; the code never raises it) the
    next `accept` returns EMFILE, `serve` retries at once forever (a busy
    loop), and the worker can no longer open its database, a log file or a
    pipe for anyone's spawn; spawn failures are environment faults
    (worker.rs:1070-1076), so the worker stops. Separately, a CONNECT
    target may hold a newline: `split_authority` rejects only `/`, `@` and
    space (egress.rs:331), so `CONNECT a\nb:443` is refused with a log
    line the sender chose, which can imitate the worker's own
    ("======== task 9 succeeded").
    *Task:* Bound the proxy: a semaphore per proxy (256 connections), an
    idle timeout of five minutes and a lifetime cap on tunnels, a 50 ms
    backoff on accept errors, `setrlimit(RLIMIT_NOFILE)` raised to the hard
    limit at worker start, control characters rejected in
    `split_authority`, and the refused line logged with `{:?}` and
    rate-limited per proxy. Test: a proxy with a limit of two refuses the
    third concurrent connection with a 503; a target with a newline is a
    400; the accept loop backs off (a test listener that errors).

15. **The host reads what the sandbox writes without a size bound.**
    `run_once` reads the agent's stdout with `BufReader::lines()`
    (agent.rs:583-586) and `run_json_phase` the same (1533): a line has no
    length limit, so an agent that prints a long line with no newline
    grows the worker's memory until the attempt's timeout (30 minutes by
    default). stderr is `read_to_string` to the end (agent.rs:573, 1523).
    `read_refused` reads the relay's file, which the sandbox writes and
    can grow, with `read_to_string` (egress.rs:663), on every attempt
    (attempt.rs:303, 625). The login files are read whole
    (login.rs:175, defect 2). Every line of stdout is also written to the
    attempt's log with no cap. The check phase alone is bounded (`Tail`,
    checks.rs:41).
    *Input:* one hostile or runaway attempt, e.g. `yes | tr -d '\n'`, or
    appending to `.git/forge-egress-refused.jsonl` in a loop.
    *Reproduction:* the worker's resident set grows with the line, or
    `read_to_string` allocates the file; the worker (all its slots) is
    killed by the OOM killer, and the attempt that caused it is requeued
    with the rest.
    *Task:* Read agent output through a bounded line reader (a cap of 4
    MiB per line, the excess dropped with a marker line in the log),
    stderr through `take(1 MiB)`, the log file with a size cap
    (`[limits] log_bytes`, default 64 MiB, then a `forge_log_truncated`
    frame and continue reading), `read_refused` through `take(256 KiB)`
    after a regular-file check, and the login files through the caps in
    defect 2. Test: a fake agent printing a 100 MiB unbroken line ends the
    attempt with the worker's memory bounded (measured by a counting
    allocator or by the log's size).

16. **Nothing bounds what an attempt consumes on the host: its tmpfs
    mounts are RAM with no size, and it has no memory, process or file
    limit.**
    `--tmpfs /tmp`, `--tmpfs /run`, `--tmpfs $HOME` (sandbox.rs:499, 506)
    take no `--size` (bwrap 0.12 has it), and each `--tmp-overlay` upper
    layer is a tmpfs too (579-587); a tmpfs defaults to half the machine's
    RAM each. There is no `setrlimit`, no cgroup and no `ulimit` in the
    wrapper (grep finds no `RLIMIT` in src), so `--unshare-pid` bounds
    what an attempt can signal, not how many processes it can start. The
    worktree is on disk and unbounded as well.
    *Input:* any attempt: a runaway build, a fork bomb, `dd
    if=/dev/zero of=/tmp/x`. *Reproduction:* the tmpfs pages are host
    memory: two attempts at 50% of RAM push the host into swap or the OOM
    killer; a fork loop exhausts the operator's process table until the
    timeout (up to 30 minutes by default) kills the group.
    *Task:* Add `--size` to every `--tmpfs` (`/tmp` 1 GiB, `$HOME` 256 MiB,
    `/run` 64 MiB; `[sandbox] tmp_bytes` to change), `ulimit -c 0 -f
    <bytes> -n 4096` in the wrapper script before `exec`, and, where
    `systemd-run --user --scope` is available, wrap the launch in one with
    `MemoryMax=` and `TasksMax=` from `[sandbox] memory_max` and
    `tasks_max` (defaults of 8 GiB and 4096); `forge doctor` reports which
    of these are in force. Replace the tmp-overlay with `--overlay` on a
    kernel-owned directory on disk if the overlay upper needs a bound.
    Test: unit tests on the argv and the wrapper text; an e2e test whose
    command writes past the tmpfs size and fails with ENOSPC instead of
    growing memory.

17. **The sandbox shares the host's IPC namespace.**
    The namespace flags are `--unshare-pid` and `--unshare-net` only
    (sandbox.rs:465-474): no `--unshare-ipc`, `--unshare-uts` or
    `--unshare-cgroup-try` (all present in bwrap 0.12's help). Reproduced:
    a SysV shared-memory segment created outside with `ipcmk -M 4096` was
    listed by `ipcs -m` inside a sandbox built with the same flags.
    *Input:* any attempt, same uid as the operator. *Reproduction:* it can
    attach segments, semaphores and message queues the operator's own
    programs (a desktop session, a database run as the operator) created,
    read what they hold and write to it; POSIX message queues are shared
    the same way.
    *Task:* Add `--unshare-ipc --unshare-uts --unshare-cgroup-try` to
    `Sandbox::command`. Test: the argv assertion in the existing
    `the_command_has_a_namespace_of_its_own…` test, and an e2e test that
    makes a segment with `ipcmk` and asserts the sandboxed command sees
    none.

18. **Operator files that hold more than the login are copied into every
    attempt, and the codex and copilot launches are not lean.**
    The wrapper copies the whole of the operator's `~/.claude.json` into
    the tmpfs home (sandbox.rs:432-453, 513-515). The CLI keeps account
    identity, per-project history and user-scope MCP server definitions
    (with their `env`) in it (the CLI's own format, not read in this
    repository). The lean-launch comment (agent.rs:841-855)
    records that before its flags the operator's connectors, skills and
    memory reached every attempt's init event; `--strict-mcp-config` keeps
    the claude CLI from *using* the file's MCP entries, not from an
    attempt reading them.
    `settings.json` is copied too (sandbox.rs:553-556) although
    `--setting-sources project,local` never reads it, so it is exposure
    only (a settings `env` block or `apiKeyHelper`). Codex's `config.toml`
    is copied (557-559) and *is* read by the CLI: `codex_common_argv`
    (agent.rs:1412-1430) has none of claude's lean flags, so the operator's
    `mcp_servers`, with their tokens, start inside every attempt.
    *Input:* any attempt at any trust level, including `public`.
    *Reproduction:* `cat ~/.claude.json ~/.claude/settings.json
    ~/.codex/config.toml` in the sandbox prints whatever the operator put
    there.
    *Task:* Seed the minimum. Write a kernel-built `.claude.json` (the
    onboarding and theme keys the CLI needs to start, nothing else)
    instead of copying the host's; stop copying `settings.json`; for
    codex, write a `config.toml` holding only the model and provider
    fields the forge provider names and start it with `-c
    mcp_servers={}` (or its equivalent); copilot keeps only the login.
    Test: a fixture host home with a secret marker in each file; assert the
    marker is in no file under the private directory and not in the
    sandbox's `$HOME`.

19. **The check phase, operations and landing verification run repository
    code with the agent's credentials and API-key environment.**
    Every launch goes through `Sandbox::command`, which seeds and binds
    the private provider directory unconditionally (sandbox.rs:540-570),
    and `agent_env` hands every command the worker's `ANTHROPIC_*`,
    `CODEX_*`, `COPILOT_*`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME`
    (agent.rs:286-311). `checks::run_one_capped` (checks.rs:177) and
    the landing's verification use `command_in` like the agent; the CLI
    never runs in them.
    *Input:* any check: the repository's `test` or `setup` command, or a
    test the agent wrote. *Reproduction:* it reads
    `~/.claude/.credentials.json` and `$ANTHROPIC_API_KEY`, and can write
    the forged pair of defect 2 or the symlink of defect 1. A check is
    also the one place with no agent to blame, and the one that runs
    after the kernel's fresh `.git` is in place (defect 3).
    *Task:* Add a phase to `Sandbox::command` (`Agent` or `Check`). A
    `Check` launch binds an empty private directory, seeds nothing, skips
    the `.claude.json` copy and gets `agent_env` without `ANTHROPIC_*`,
    `CODEX_*`, `COPILOT_*`, `CLAUDE_CONFIG_DIR` and `CODEX_HOME` (a
    repository that needs one declares it for that check in forge.toml).
    `checks.rs`, `operation.rs` and the landing pass `Check`. Test: a check
    whose command is `env; ls ~/.claude` shows none of them, and the agent
    launch is unchanged.

20. **The private provider binds shadow anything the launch put under the
    default directories, and `CODEX_HOME` is passed through to a directory
    that is not bound.**
    The read-only binds for the agent binary's directories and
    `extra_ro` come first (sandbox.rs:510-512); the private binds over
    `$CLAUDE_CONFIG_DIR`, `~/.codex` and `~/.copilot` come later
    (560-570) and hide whatever was mounted beneath them. Reproduced: a
    directory bound read-only at `$HOME/.claude/local`, then a private
    directory bound at `$HOME/.claude`, lists empty. Claude's local
    install lives at `~/.claude/local/claude`. Separately, `agent_env`
    lets `CODEX_HOME` through (agent.rs:296) and `codex_dir` is always
    `~/.codex` (sandbox.rs:285): with `CODEX_HOME` set on the host, codex
    inside looks in a directory that does not exist in the tmpfs home and
    has no login; claude is handled (`CLAUDE_CONFIG_DIR`, sandbox.rs:282-284).
    *Input:* an agent binary or a `ro_paths` entry under one of those
    directories; or `CODEX_HOME` set. *Reproduction:* the launch dies
    "not found" (or with no login) in every sandbox on that host, before
    the agent starts.
    *Task:* Detect both in `Sandbox::detect`: fail with a message naming
    the path when an `agent_dirs` or `extra_ro` entry is under a
    directory that will be bound over, and bind such an entry after the
    private binds instead. Read `CODEX_HOME` for `codex_dir` (and the
    copilot equivalent) the way `CLAUDE_CONFIG_DIR` is read. Test: unit
    tests on the ordering with an agent under `~/.claude/local`, and on
    `codex_dir` with `CODEX_HOME` set.

21. **The model allowlist is every configured provider's endpoints, for
    every attempt, including endpoints only the worker itself talks to.**
    `model_rules` (egress.rs:165-186) is built from `home.providers`
    (ctx.rs:199-204) and `Sandbox::policy_for` adds all of it to every
    worktree's policy (sandbox.rs:355-369), even at `egress = "model"`.
    A claude attempt may therefore reach `*.openai.com`, `chatgpt.com`,
    `*.githubcopilot.com`, `api.github.com` and every provider's
    `base_url` and every URL in its `env`. The `chat` and `jev` runners
    post from the worker process (agent.rs:1003, agent/jev.rs:227),
    never from a sandbox, yet `Runner::Jev` opens `api.cloudflare.com`
    (egress.rs:179), the account-management API, and a `chat` provider's
    LAN address (`dev.home:11434`) is an exact rule, so unchecked
    (egress.rs:352-376).
    *Input:* any attempt. *Reproduction:* `curl https://api.github.com/`
    from a claude attempt succeeds when a copilot provider is configured;
    a public-trust attempt can reach the LAN model box. Each is a route
    for anything it can read, using a key it brings.
    *Task:* Make the model allowlist the attempt's own provider's: give
    the launch's `Policy` the endpoints of the provider its step resolved
    (`Sandbox::set_provider_hosts(worktree, rules)` beside `set_egress`,
    called where `f.allow_egress` is), not the union; leave `Chat` and
    `Jev` out (they run on the host); drop `api.cloudflare.com`. Test:
    a unit test that a claude worktree's policy has no openai host when a
    codex provider exists, and that a chat provider's URL is in no
    sandbox policy.

22. **An environment grant is applied to the task's worktree only, so the
    tests step's own directories never see it.**
    `apply_environment` grants for `t.worktree` (engine.rs:445-470, the
    calls at 451 and 461), and `Sandbox::policy_for` and the read-only
    binds look up the launch directory and its ancestors
    (sandbox.rs:355-369, 592-598). The tests contract runs in
    `<worktree>-tests` and its red-on-base check in `<worktree>-red`
    (attempt.rs:170, 367-373; verify.rs:1252-1257): siblings, not
    descendants.
    *Input:* a tests-step failure on a covered need in the scratch or
    tests clone (a Playwright cache path, a registry host).
    *Reproduction:* the grant is recorded for the worktree, "running
    again, nothing counted"; the rerun fails in the same directory with
    the same line; `apply_grant` now reports the grant already applied and
    the environment result is `Left`. One decision row, one wasted
    rerun, and a failure that the policy said it had covered.
    *Task:* Key grants (and `declared`, `caches`) by task, not path:
    register a task's directories once (`worktree`, `tests_clone_dir`,
    `scratch_dir`) with the `Sandbox`, and have the three maps resolve any
    of them to the task's entry. Test: grant a host for a worktree and
    assert `policy_for` of its `-tests` and `-red` directories carries it.

23. **`<worktree>-red`'s provider directory is never discarded, and every
    launch scans every provider directory ever left, under the login lock.**
    `red_on_base` removes the scratch directory (verify.rs:1257, 1294) but
    not `<scratch>-provider`, which its sandboxed checks created
    (sandbox.rs:540-570); `forge gc` discards the worktree's and the
    tests clone's (cli/gc.rs:45-49) and not `-red`. Each holds a copy of
    the login. `private_copies` matches every `*-provider` directory under
    the worktrees directory (login.rs:205-222) and `seed` and
    `refresh_on_host` write back from each of them while holding the lock.
    *Input:* any task on the tests contract. *Reproduction:* one leaked
    directory (and login copy) per such task; after a few hundred, each
    launch reads a few hundred files under the lock that every other
    launch waits on (defect 10).
    *Task:* Discard `<scratch>-provider` where `red_on_base` removes the
    scratch, and in `gc`; and have `private_copies` skip a provider
    directory whose worktree no longer exists after writing it back once.
    Test: a unit test over a tempdir with a stale `-provider` and no
    worktree, and one that `red_on_base` leaves nothing behind.

24. **The proxy directory is a predictable name in the shared temp
    directory, and an existing one is accepted.**
    `own_dir()` is `temp_dir()/forge-egress-<pid>` (egress.rs:779-781);
    `socket_for` creates it with `DirBuilder::recursive(true).mode(0o700)`
    (egress.rs:742-748), which succeeds on a directory that already exists
    and leaves its owner and mode alone (reproduced: an existing 0777
    directory stays 0777), and `bind` removes any file at the socket's
    name first (268-271).
    *Input:* another local user creates `/tmp/forge-egress-<pid>` for a
    pid the worker is about to get (pids are sequential; a range costs
    nothing) with mode 0777. *Reproduction:* the worker binds its
    sockets in the attacker's directory; the attacker, who owns it,
    replaces `p0.sock` with a listener of their own; the sandbox binds
    that socket in as its only route out (`check_socket` tests existence
    only, sandbox.rs:411-422), so the policy is theirs to grant and all
    plain-HTTP traffic is theirs to read. On a single-user machine this
    is inert; the kernel otherwise takes care to keep sessions private.
    *Task:* Put the proxy directory under `FORGE_HOME/run/egress-<pid>`
    (or `$XDG_RUNTIME_DIR`), create it with plain `create_dir` (fail on
    `EEXIST`, after removing a dead pid's), and verify with `lstat` that
    it is ours (`uid == geteuid()`, mode without group or other bits) before
    the first `bind`; update `sweep_dead`, doctor and the successor e2e
    test (tests/e2e/successor.rs:441) to the new path. It also leaves the
    directory outside the reach of a tmp cleaner. Test: a unit test that
    a pre-existing 0777 directory is refused and that the sweep still
    keeps a live pid's directory.

25. **The reviewer runs with the coder's provider state: its session
    transcripts are readable, which the review is designed not to see.**
    `provider_state_dir` is keyed by the worktree alone (sandbox.rs:
    198-200), it "lives as long as the task, not one attempt"
    (188-197), and the review contract runs in `t.worktree`
    (attempt.rs:242) with `journal: None` "told nothing of earlier
    attempts: it judges the branch as it stands" (attempt.rs:245-246).
    The CLI's session transcripts (its own layout, not read in this
    repository) are under the private `.claude/projects/`. The tests step is properly apart: its clone has a
    provider directory of its own.
    *Input:* a `reviewed` task: coder attempt, then reviewer.
    *Reproduction:* the reviewer, an agent with `Bash` and `Read`, lists
    `~/.claude/projects/` and reads the coder's whole transcript: its
    claims, its reasoning, the checks it ran.
    *Task:* Give each step kind a provider directory of its own beside the
    task's: `<worktree>-review-provider` for `Contract::Review` (still
    ending `-provider`, so `private_copies` finds it), seeded the same
    way, discarded with the worktree in `discard_provider_state`. Test: a
    unit test that the review directory is distinct and is removed
    together with the coder's.

26. **The codex and copilot prompts are command-line arguments.**
    `run_codex` appends `l.prompt` to argv (agent.rs:1685), copilot passes
    it after `-p` (agent.rs:1883-1884); claude reads it from stdin
    (agent.rs:567). The launch is `bwrap … sh -c script sh <argv>`, so the
    prompt is in the process's command line, visible to every local user
    in `ps` and `/proc/<pid>/cmdline`; and a single argument is capped by
    the kernel at 128 KiB (`MAX_ARG_STRLEN`), with nothing in prompts.rs
    bounding the total (its truncations are per item).
    *Input:* any codex or copilot launch; a task text, plan, journal and
    context above 128 KiB. *Reproduction:* the task text and the coder's
    journal are on the host's process list; a long enough prompt makes
    `execve` fail (E2BIG), which `spawn_retrying_etxtbsy` returns as a
    spawn error and the engine treats as an environment fault, stopping
    the worker.
    *Task:* Feed the codex prompt on stdin (`codex exec -` reads it there)
    and check what copilot accepts (a prompt file, stdin) for the same;
    where it accepts only `-p`, cap the prompt below 100 KiB with a visible
    truncation note and record the limit in docs/EXECUTION.md. Test: a
    fake codex that fails when its argv is longer than 1 KiB, run with a
    200 KiB prompt.

27. **The refresh window is a constant 30 minutes, and an attempt's
    timeout is not.**
    `REFRESH_WINDOW_MS` is 30 minutes (login.rs:30), and a launch
    refreshes the host login only when it is inside it
    (refusal.rs:58, 86). A task's `--timeout-secs` defaults to 1800
    (cli/mod.rs:83-86) and is settable to any value (queue.rs:426, 505);
    codex and copilot give each phase the whole timeout
    (agent.rs:1527).
    *Input:* concurrent attempts, at least one with a timeout past 30
    minutes, launched with a token that has 31 minutes left. *Reproduction:*
    each is seeded with the same pair, the token expires under them at
    the same instant, each refreshes with the same refresh token (the
    doc at login.rs:3-9 says it rotates), the first wins and the others'
    CLIs fail to refresh and empty their private file; the winner's is
    written back. With the default timeout the margin is one minute.
    *Task:* Compute the window per launch as `max(30 min, timeout +
    check timeout + 5 min)` and pass the launch's timeout to
    `login_problem`. Test: a unit test over `near_expiry` with a
    two-hour timeout and a token with 90 minutes left.

### 1.2 Read and found sound

So the next reader does not repeat it:

- **The rule matcher.** Suffix rules match below the domain and never the
  domain itself or a lookalike (`evilcrates.io`, `x.crates.io.evil.com`,
  egress.rs:114-131, tested at 856-880); `*.com`, a bare `*`, URLs, IPv6
  literals and `@` are refused at parse (63-111); default ports are 443
  and 80 and a rule with a port pins it; an exact rule outranks a suffix
  one. Where a suffix-matched name resolves is checked, and the address
  that was checked is the one connected to, so DNS rebinding is closed for
  suffix rules (`dial`, 352-376; `is_public`, 234-261, which also unwraps
  v4-mapped v6).
- **The request head.** Bounded at 16 KiB and 10 seconds (263-265, 383-403);
  authorities with `/`, `@` or a space are refused (331); plain HTTP is
  forwarded origin-form with `Connection: close`, and `Proxy-Authorization`
  and hop-by-hop headers are dropped (486-503). A pipelined second request
  goes only to the host already allowed, so it cannot reach another.
- **A namespace of its own.** `--unshare-net` is present whether or not a
  route exists (sandbox.rs:468, tested at 957-1004); bwrap brings up
  loopback; the relay is a child in the namespace and dies with it, and
  with `--die-with-parent` and `--new-session` a killed or crashed worker
  leaves no sandbox process, no mount (they live in the mount namespace)
  and no listener. Every spawn is `kill_on_drop` (agent.rs:557, 1514;
  checks.rs:185) and a check's group is killed after it exits
  (checks.rs:218-227).
- **The proxy directory's lifecycle since 723 and 755.** One directory per
  process, never wiped by a `Proxies`, removed once at worker exit
  (`OwnDirGuard`); a `Proxies`' drop removes only its own sockets, and
  socket names come from a process-wide counter, so the second `Sandbox`
  a reload builds cannot collide with the first (egress.rs:717-776, 754);
  `sweep_dead` removes only `ESRCH` pids, never its own, and only real
  directories (`DirEntry::file_type` does not follow a link, 806-826), so
  a successor cannot remove a draining predecessor's (worker.rs:899-905;
  tests/e2e/successor.rs:441-442). A launch whose socket file is gone is an
  environment fault naming it, for agent runs (defect 11 is about the
  rest). Left behind on a crash and tolerable: the directory itself (swept
  by the next worker or `forge doctor`), `<worktree>-provider` copies (0600
  files, see defects 9 and 23), `.forge-credentials.lock` and
  `.forge-writeback` beside the host login, and, for the ssh backend, the
  remote's `/tmp/forge-executor.*` when the local script is `SIGKILL`ed
  (its `trap … EXIT` does not run, executor.rs:134).
- **The credential lock and the file swap.** `flock` on an open file holds
  against threads and processes (login.rs:146-160); `replace_atomic`
  writes a `create_new` 0600 sibling, `fsync`s and renames (117-144);
  `should_write_back` never lets an emptied host file or an expired private
  one win (109-115); `seed` removes a stale private copy when the host is
  empty rather than seeding a dead pair (225-234); an empty host token is a
  provider refusal, not an attempt (refusal.rs:56-68, 140-170). What is
  wrong is what the sandbox can feed it (defects 1, 2), not the mechanics.
- **What a launch sees of the host.** The tmpfs `$HOME` hides `~/.ssh`,
  `~/.aws`, `~/.gitconfig` and the registered checkout and its `.git` (the
  clone has its remote removed, git.rs:209); `env_clear` plus an
  allowlist keeps `GITHUB_TOKEN`, `AWS_*`, `SSH_AUTH_SOCK` and the rest of
  the worker's environment out (agent.rs:286-311; the prefixed families
  are defect 19); `HOME` is forced (sandbox.rs:612); the operator's real
  `.claude`, `.codex` and `.copilot` are never a bind source (asserted at
  sandbox.rs:748-756); the package caches are overlays whose writes are
  discarded, and a bwrap without overlay support binds none rather than
  binding read-write (579-587).
- **Environment grants' own guards.** A need's path never widens a policy
  grant, because `covers` returns the table's directory (environment.rs:
  337-343); the supervisor's ceiling is code, not the prompt, and refuses
  `..` (405-431); a host grant needs a trust level that reaches declared
  hosts (ctx.rs:312, env_supervisor.rs:84-96); refusals count only after a
  failed check and never from hosts that succeeded attempts were refused
  too (environment.rs:99-122); the recorded refusal file is best effort
  and never breaks the connection (egress.rs:646-658). Defect 6 and 7 are
  the holes.
- **The host executor.** `Host::command` is `env_clear` plus the explicit
  environment and the worktree as cwd, no shell (executor.rs:63-76);
  `ssh_command` shell-quotes every argument and value and relocates paths,
  and `ssh_destination` refuses a leading `-` and anything outside
  `[A-Za-z0-9._-]` (executor.rs:95-146, config.rs:100-113), so neither
  injects options. The two backends differ where they must: no write-back,
  no socket check, no policy (defect 5 is the consequence).
- **Two attempts of one task.** Never concurrent: claiming is exclusive
  (REVIEW-3 1.2), a requeued task's killed sandbox is gone before the next
  claim, and each launch reseeds credentials and settings while keeping the
  session directory, which a `--resume` needs (sandbox.rs:188-197). What
  they share is defect 25's.
- **Claude's lean launch.** `--strict-mcp-config`, `--disable-slash-
  commands`, `--setting-sources project,local` and
  `--exclude-dynamic-system-prompt-sections` are unconditional
  (agent.rs:841-880); the residual is that project settings may apply
  (defect 4) and that codex and copilot have no equivalent (defect 18).
- **Host residuals not called defects.** `/etc` is bound whole and
  read-only (sandbox.rs:476-485): world-readable files there, and
  `/etc/machine-id`, are visible; the default package caches include
  `~/.npm`'s `_cacache` and `~/.cargo/git`, which hold whatever private
  packages the operator has fetched; `FORGE_CACHE_DIR` is per repository,
  not per trust level, but only the graph the operations rewrite on every
  run lives there today (docs/ACTIONS.md:302, 357); one proxy per distinct
  policy lives as long as the worker (a few file descriptors each). All are
  the operator's to decide; none needs a task.

### 1.3 Closing table

The fix initiative is filed from this table, one task per row, in this
order. Size is S (an afternoon, one file and its test), M (a day, two or
three files), L (more).

| # | Defect | Where | Severity | Size | Depends on |
|---|--------|-------|----------|------|------------|
| 1 | Seed copies write through a sandbox-planted symlink; host login emptied | login.rs:232, sandbox.rs:553-569, agent.rs:1654 | high | S | none |
| 2 | Write-back accepts any file the sandbox wrote | login.rs:109-187, sandbox.rs:384, sandbox.rs:46-52 | high | M | 1 |
| 3 | Host git runs unhardened on a sandbox-written `.git` | git.rs:754-933, attempt.rs:609, agent.rs:1716, supervisor.rs:526, assess.rs:163 | high | M | none |
| 4 | Refresh probe runs the CLI on the host in the worktree | refusal.rs:114-136, agent.rs:325, executor.rs:63 | high | S | none |
| 5 | Model-only trust levels run unsandboxed without a refusal | ctx.rs:268, executor.rs:153, 235, queue.rs:190 | high | M | none |
| 6 | Cache grants ignore trust; ceiling is any `~/.cache` dir | ctx.rs:312-316, env_supervisor.rs:88, environment.rs:405 | medium | S | none |
| 7 | Ceiling admits IP literals; granted exact rules skip the rebinding check | environment.rs:186-403, egress.rs:362 | medium | S | none |
| 8 | Codex and copilot logins seeded, never written back | sandbox.rs:557-569, login.rs | medium | M | 1, 2 |
| 9 | Write-back failure silent; discard without write-back | sandbox.rs:388, login.rs:228, gc.rs:45, landing.rs:157 | medium | S | 2 |
| 10 | Blocking `flock` on runtime threads; lock held across the probe | login.rs:150, sandbox.rs:548, refusal.rs:76 | medium | M | 4 |
| 11 | Broken route proceeds; socket check covers agent runs only | sandbox.rs:411, agent.rs:795, egress.rs:726 | medium | M | none |
| 12 | Relay start unchecked; `(deleted)` relay path | sandbox.rs:289, 441, ctx.rs:194, agent.rs:275 | medium | S | none |
| 13 | Transient bwrap retry covers the claude agent only | agent.rs:777, 912, checks.rs:177 | medium | S | none |
| 14 | Proxy: no connection or idle bounds, spinning accept, log injection | egress.rs:274, 310, 331, 440 | medium | M | none |
| 15 | Unbounded host reads of sandbox output | agent.rs:571-586, 1523-1533, egress.rs:663 | medium | S | none |
| 16 | No resource bound: RAM tmpfs, pids, rlimits | sandbox.rs:499, 506, 579 | medium | M | none |
| 17 | Shared IPC namespace | sandbox.rs:465-474 | medium | S | none |
| 18 | Whole operator config files copied in; codex not lean | sandbox.rs:432, 513, 553-559, agent.rs:1412 | medium | M | none |
| 19 | Checks run with agent credentials and API-key env | checks.rs:177, sandbox.rs:540, agent.rs:286 | medium | M | 1, 2 |
| 20 | Private binds shadow agent dirs; `CODEX_HOME` not bound | sandbox.rs:510-570, 285, agent.rs:296 | medium | S | none |
| 21 | Model allowlist is every provider's, host-side runners included | egress.rs:165-186, sandbox.rs:355 | low | S | none |
| 22 | Grants keyed to the worktree miss `-tests` and `-red` | engine.rs:451, sandbox.rs:355, 592 | low | S | none |
| 23 | `-red-provider` never discarded; every launch scans all copies | verify.rs:1257, gc.rs:45, login.rs:205 | low | S | 10 |
| 24 | Predictable, unverified proxy directory in the shared temp | egress.rs:742, 779, 268 | low | S | none |
| 25 | Reviewer reads the coder's session transcripts | sandbox.rs:198, attempt.rs:242 | low | S | none |
| 26 | Codex and copilot prompts in argv | agent.rs:1685, 1883 | low | S | none |
| 27 | Refresh window ignores the attempt timeout | login.rs:30, refusal.rs:58 | low | S | none |

Order: 1, 4, 3 and 5 first (each is an untrusted run reaching the
operator's host or login, and none needs the others); 2 right after 1;
6, 7, 11, 12, 13 and 20 are independent and small; 10 before 23; 8 and 19
after 2 so they inherit its guards. Rows 15, 16 and 17 take the same
file (sandbox.rs, agent.rs) as 13 and 20: file them one after another or
merge them into one branch.

## 4. The weekly measurement caught up with itself (2026-09-27)

This part was written when `engineering-weekly` fired, before the edges
were read; it is kept as it was, its sections renumbered under 4.

`.forge/workflows/engineering-weekly.toml` fired on schedule and found what
it exists to find: a src file over its 3000-line bound,
`src/workflows.rs` at 3671. The third review, a themed cold read of the
concurrent paths, landed the day before this one fired (2026-09-26); the
last measurement-shaped review was the second, on 2026-09-18, when the
tree stood at 41,211 kernel lines in 44 files. In the nine days between,
513 commits landed, unevenly (48 on the 21st, 156 on the 22nd, 11 on the
24th), and the kernel grew to 69,589 lines. This week's numbers: the five
largest files are workflows.rs (3671), agent.rs (3471), job.rs (2930),
verify.rs (2178) and engine.rs (1987); the eight longest functions run
from `engine.rs`'s `run_directive_step` at 533 lines down to
`view/projects.rs`'s `portal_doc` at 275; 36 modules carry no
`#[cfg(test)]` block; 561 unit and 377 e2e tests run the suite in 83
seconds; clippy is clean at 0 warnings. This review reads the file that
crossed, and the job that caught it, and records only what was confirmed
at its line.

### 4.1 Findings confirmed while reading

1. **workflows.rs is already half split, and what's left divides the
   same way the first half did.** `mod edges`, `judgment`, `library` and
   `shadow` (workflows.rs:21-24) already carry 798 lines out of the file
   in four pieces with no behaviour shared between them. What remains,
   2522 non-test lines (1-2522) plus a 1149-line `mod tests`
   (2523-3671, 31% of the file), still divides the same way: the
   schema and its parsing (`Contract` through `parse_workflow`,
   38-1341, including the two built-in catalogs and `parse_action` at
   171 lines and `parse_workflow` at 116); the filesystem catalog
   loader (`Catalog`, `load_catalog`, `load_actions`, `load_all`, `get`,
   1441-1576); composition and resolution (`splice`, `check_flow`,
   `resolve`, `job_steps_into`, `resolve_job`, `resolve_job_in_repo`,
   1578-1905); reading a workflow from a *pinned git commit* rather
   than the live catalog (`ls_tree_dir`, `show_at`, `load_all_at`,
   `resolve_job_at`, `resolve_job_for_project`, `fixtures_at`,
   1912-2085) — a genuinely separate dependency (git plumbing) from the
   fs-catalog block above it; and the authoring-time checks
   (`validate_repo`, `lint_catalog`, `lint`, `resolve_jobs_in_tree`,
   `check`, `uncommitted`, `commit_for`, 2155-2520). None of the five
   calls into another's internals beyond the shared `Workflow` and
   `ActionDef` types, the same property that let edges, judgment,
   library and shadow move cleanly.

2. **The job that caught this crossing does not act on seven of the
   eight numbers it measures.** `measure.sh` writes `longest_functions`
   into `measurements.json` (an awk brace-depth scan over every `fn` in
   `src/`), but `compare-thresholds.sh` never reads that field: it
   recomputes `run_task`'s own length with a second, independent awk
   pass over `src/engine.rs` and compares only that number against
   `RUN_TASK_MAX_LINES`. `run_task` is 171 lines (engine.rs:188-358),
   comfortably inside its 400-line bound, four measurement cycles after
   docs/REVIEW-2.md's stage 4 shrank it from 1082. But
   `run_directive_step` — the function that stage 4 deliberately left
   holding "the step loop" when it carved `retry_start`, `escalate` and
   `finish` out of `run_task` — is now 533 lines (engine.rs:796-1328,
   matching measurements.json's own figure exactly), the single largest
   function in the kernel, ahead
   of `supervisor.rs`'s `supervise` (460) and `job.rs`'s `run_now`
   (452). All three are measured every week and none is compared
   against anything.

3. **Two of the four modules already split out of workflows.rs carry no
   `#[cfg(test)]` of their own, and the measurement can't tell that from
   genuinely untested code.** `modules_without_tests` names
   `src/workflows/edges.rs` and `src/workflows/shadow.rs` (spot-checked:
   confirmed, alongside `src/agent/inputs.rs`, `src/cli/demo.rs`,
   `src/env_supervisor.rs` and `src/successor.rs` from the same list).
   Both are exercised indirectly through workflows.rs's own suite —
   `inline_composition_splices_and_rejects_cycles` (workflows.rs:2708)
   drives `edges::resolve`'s cycle rejection, `builtins_resolve_and_
   carry_blob_hashes` (workflows.rs:2587) drives `shadow::text_blob_
   hash` through `parse_action` — but a `grep -L '#\[cfg(test)\]'`
   measurement has no way to see that, so the same two names will
   reappear on this list every week even if they are never the reason a
   bug ships.

4. **Two more of the five largest files carry the same shape as
   workflows.rs, and one is a single crossing away.** agent.rs (3471)
   is 38% inline test (`mod tests` from line 2154, 1318 of 3471 lines);
   job.rs (2930) is roughly a fifth (`mod tests` from line 2339). The
   `kernel_lines` and `largest_files` measurements count both in the
   same column as parsing and control flow, so part of what crossed
   this week's threshold is 561 unit tests earning their keep, not new
   logic — worth knowing before a split is sized, not a reason to skip
   one.

### 4.2 A number this review can't explain

`e2e_wall_time_secs` is 83. The second review's suite ran 195 e2e tests
in 22 seconds; its outcome, after 105 more tests landed the same day,
still ran in 24. This week's suite, 377 e2e tests, takes over three
times as long per test as either of those points. Nothing in this read
found the cause — it would take profiling the suite, not reading
workflows.rs, to say whether it's the sandboxed checks the second review
already named as real host dependencies (bwrap, loopback servers,
headless Chromium) growing in number, a slower shared fixture, or
something waiting on a timer that used to return sooner. Recorded so the
next reader has the baseline and isn't the first to notice it.

### 4.3 Keep

- **`run_task` held its own bound.** Four measurement cycles after
  docs/REVIEW-2.md's stage 4, it is 171 lines against a 400-line cap,
  the one function the weekly job actually watches.
- **Clippy is clean.** 0 warnings at `-D warnings`, same as every prior
  review.
- **The split workflows.rs already did holds up.** edges.rs,
  judgment.rs, library.rs and shadow.rs have not grown back into
  workflows.rs; nothing in this read found a caller reaching around
  them into workflows.rs internals or vice versa.
- **The measurement itself is sound where it's read.** `largest_files`,
  `modules_without_tests` and `clippy_warnings` all matched what this
  read confirmed by hand; the gap is that `longest_functions` is
  computed and then set aside (finding 2).

### 4.4 The plan

Ordered so stage 0 is cheap and stops the same gap from reopening next
week.

**Stage 0. Make the job compare what it measures (hand).**
`compare-thresholds.sh` should read `.longest_functions[]` from
`measurements.json` instead of re-deriving `run_task`'s length with its
own awk pass, and gain a `FUNCTION_MAX_LINES` `[env]` default (400, matching
`RUN_TASK_MAX_LINES`) so any function over the bound names itself in the
filed task, `run_task` included. `run_directive_step` should cross it on
the next run without anyone reading engine.rs first.

**Stage 1. run_directive_step's own seams (hand, after 0 or in
response to it).** Split the same way `run_task` was split in
docs/REVIEW-2.md's stage 4: the per-step attempt loop, the
provider-hold wait, and the refund and rewind bookkeeping into named
functions or `StepFlow` variants, each unit-tested on its inputs, target
a few hundred lines. The discipline already exists once; this is
applying it a second time to the piece that inherited the first
function's size.

**Stage 2. workflows.rs's remaining five (Forge, after 0 so the guard
exists to hold the result). One task per concern, no behaviour change,
each one's own tests moving with it (unlike edges.rs and shadow.rs,
whose tests stayed behind — finding 3): schema and parsing into
`workflows/schema.rs`; the filesystem catalog loader into
`workflows/catalog.rs`; composition and resolution
(`splice`/`check_flow`/`resolve`/`job_steps*`/`resolve_job*`) into
`workflows/compose.rs`; the pinned-commit git reader
(`ls_tree_dir`/`show_at`/`load_all_at`/`resolve_job_at`/`fixtures_at`)
into `workflows/pinned.rs`; the authoring-time checks
(`validate_repo`/`lint`/`resolve_jobs_in_tree`/`check`) into
`workflows/lint.rs`. Add a size guard beside the split, the way cli.rs's
function-length test guards its own stage 3, so the next feature can't
silently rebuild the monolith one workflow concern at a time.

**Stage 3. A handful of the 36 (Forge, optional, low cost).** Most of
`modules_without_tests` are pure-function modules already exercised
through an e2e path, the same finding docs/REVIEW-2.md's stage 6 made
before adding tests to eight of them. Pick the ones with real branching
(not `src/workflows/edges.rs` or `shadow.rs` — finding 3 says why not
yet) and give them the same treatment, no new fakes.

### 4.5 What this buys

Stage 0 is the smallest fix that matters most: it turns a measurement
that already runs every week into a check that catches the next
`run_directive_step` before a reader has to find it by hand. Stage 1
returns the run's step loop to the size the second review's stage 4 put
it at. Stage 2 finishes what edges, judgment, library and shadow started,
with a guard so it stays finished. None of it changes what a workflow or
an action does; every stage is checked by the same 561 unit and 377 e2e
tests.

## 6. Edges, section 2: plugin supervision and the successor handoff (2026-09-27)

This is the second of the edges reads (the initiative filed from this
review's closing table); the first covers the sandbox and the egress
proxy. Same rules as section 1: a finding is recorded only when it was
confirmed at its line, with the input that reaches it, what goes wrong and
a task text a follow-up can be filed from; what was read and found sound
is listed at the end. Read as one system: `src/plugins.rs`,
`src/successor.rs`, `src/release.rs`, the plugin and drain paths of
`src/worker.rs` (`work`, 907-1123), `src/store/workers.rs`,
`Store::open`/`migrate`/`apply_contracts` (`src/store/mod.rs`),
`report::log` and the reference plugins' event loops. Nothing was run
against the operator's machine: the 118 orphaned plugin processes from
test runs could not be counted from here, so finding 10 is derived from
the code paths that produce them, not from the process table. No code was
changed.

Who owns a plugin process today, so the findings can be read against it:
the worker is its parent (a tokio `Child`), the plugin is its own process
group leader (`spawn_plugin`, plugins.rs:505-532, `process_group(0)`), it
stays in the worker's session and cgroup, it has no parent-death signal,
and `kill_on_drop(true)` (plugins.rs:526) reaches only the leader. What
dies with the worker: on SIGTERM/SIGINT, everything, after the drain
(`Supervisor::stop`, worker.rs:1108); on the double-signal abort, the
leader only (finding 10); on SIGKILL, the OOM killer or a crash, nothing,
unless systemd's cgroup cleanup does it (`KillMode=mixed`, init.rs:79,
which is why no crashed worker's plugins were found on the machine).

### 6.1 The successor handoff

1. **A predecessor that stops its plugins for a successor which then
   exits before claiming never starts them again (2026-09-27 21:25,
   task 818).** `superseded` takes the supervisor (`plugins.take()` and
   `p.stop().await`, successor.rs:119-121) *before* it spawns the
   successor, and puts it back only if the spawn itself fails (135). If
   the spawn succeeds and the successor dies later, the next pass reaps
   it (106-112), inserts its release into `failed`, finds no newer live
   worker and returns `Ok(false)` at 139: the worker claims again, with
   `plugins` still `None` for the rest of its life. Input: any successor
   that exits between its spawn and its first claim (a bad release, a
   failed migration, a stop job, `kill`). What goes wrong: every enabled
   plugin is stopped for as long as the worker lives; the run states say
   "stopped by worker", nothing is wrong to the worker, and `forge
   doctor` reports the plugins row `ok` (finding 18). That is the evening
   of the 27th. *Task:* in `Succession::superseded`, when the child has
   exited and no newer worker is live, and the caller's supervisor is
   `None` for a daemon worker, start a new `Supervisor` (the same call as
   successor.rs:135), and close the dead successor's row with
   `stop_worker` (finding 2); put the restart behind a small function
   both paths use, and add an e2e beside `tests/e2e/successor.rs` that
   stages a release whose `forge` exits immediately, with an enabled
   plugin, and asserts the plugin's run state is `running` again and the
   worker claims.

2. **The dead successor's row in `workers` is never closed, and
   `live_workers` trusts the bare pid.** A registers the child's row at
   successor.rs:124 and the child registers itself (`join`, 65); only
   the process itself calls `stop_worker` (`leave`, 227), so a killed
   successor's row stays open. `live_workers` (store/workers.rs:55-69)
   keeps a row when `pid_alive(pid)` (kill 0, worker.rs:40) says the pid
   exists. Once the pid is reused by any process (or the row's pid is
   another user's, which `EPERM` counts as alive), the row is a live
   worker of a newer version: the old worker sees `w.id > self.id &&
   w.version != self.version` (successor.rs:114-116), claims nothing and
   exits when its attempts end, `staged_successor` refuses the release
   because a "live worker" runs it (156), and `apply_contracts` never
   runs (store/mod.rs:729-735). Task ownership already solved this
   (`store::start_of`, worker.rs:889, the identity no reused pid
   shares); the workers table did not take it. *Task:* record
   `start_of(pid)` on the `workers` row (an additive migration), have
   `live_workers`' callers compare it the way orphan recovery does, and
   have `superseded` call `stop_worker` for a child it reaped.

3. **Any exit of the successor, a clean drain included, makes the
   predecessor claim again.** successor.rs:106-112 treats every
   `try_wait` result as "the successor died". Under the stop job the
   docs describe (OPS.md, "The running binary"): the operator's
   `systemctl --user stop forge-worker` sends SIGTERM to the main pid,
   which after `MAINPID=` is the successor (`KillMode=mixed` signals only
   the main process until the timeout, as OPS.md and doctor.rs:633-636
   describe; not reproduced under systemd here); the successor drains and
   exits 0;
   the old worker, still draining a running attempt and in the unit's
   cgroup but no longer main, reaps it, finds no newer live worker and
   claims new tasks while the unit is `deactivating`, until
   `TimeoutStopSec` (2400 s) SIGKILLs it and whatever it claimed.
   `stop_requested` (187-195) only covers the successor's own view. Input:
   a stop or restart while a predecessor still holds attempts. *Task:*
   `superseded` must distinguish "exited without taking the unit over"
   (the capability file does not name it, `read_capability`, 243) from
   "exited after it did": in the second case the predecessor must stay
   draining and exit, not claim; add an e2e that stops the successor
   cleanly while the predecessor holds a running attempt and asserts no
   new claim by the predecessor.

4. **A worker that is stopping still starts a successor.** `work` calls
   `succession.superseded` on every pass (worker.rs:947) and reads
   `stopping` only afterwards (948, and in the claim guard at 957);
   `superseded` never sees `stopping`, and `staged_successor` (118) only
   asks whether a release is staged and not running. Input: SIGTERM (or
   Ctrl-C on a hand-run `forge work`) to a worker while `bin/staged`
   names another release. What goes wrong: the stop is answered by a new
   detached worker in its own process group (successor.rs:178) that flips
   `current`, restarts `forge-web` and `forge-portal` (291-306) and
   claims; on a hand-run worker nothing tells it to stop, and under
   systemd it stops only because `stop_requested` happens to see
   `deactivating`. *Task:* pass `stopping` into `superseded` (or check
   it before the call) so that a stopping worker starts no successor,
   and add a worker test that signals a worker with a staged release and
   asserts no second worker registers.

5. **`current` is flipped, and web and portal restarted, before the
   successor has proved itself; nothing puts them back when it dies.**
   `join` runs `take_over` (successor.rs:72; flip and `systemctl
   restart`, 291-306) first, then `MAINPID`/`READY` (74), the capability
   file (75) and `apply_contracts` (76). A successor that exits after 72
   leaves `current` on the dead release; `superseded` records the
   failure (106-112) and never flips back. The unit's `ExecStart` is
   `<home>/bin/current/forge` (init.rs:150-157, 79), so the next start of
   the unit runs the release that just failed: if the successor dies after
   `MAINPID=` (74) systemd sees its main pid exit and `Restart=on-failure`
   starts that release ten seconds later, and an operator's restart or a
   crash of the old worker does the same, up to five times in three
   hours (`StartLimitBurst=5`, init.rs:69-70) until the unit is refused. The same holds one step earlier for the schema: `forge
   work` opens the store (cli/tasks.rs:344, `Store::open`) and migrates it
   before `join`, and a release older than the schema refuses to open it
   (store/mod.rs:782, "newer than this forge"), so after a failed
   successor every fresh process of the old release (a plugin's `forge`
   call through `current`, a restarted worker) fails on the store until
   someone stages a release at least as new. Input: a successor that
   passes migration and then dies (or is killed) before claiming.
   *Task:* make the flip the last step of the handoff: run `take_over`
   after the capability file is written and after the successor's first
   completed pass, or have the predecessor call `release::restore`
   (release.rs:115) with the pointers it read when it reaps a successor
   that never took the unit over; decide and document what the old
   binary does with a schema newer than itself (docs/OPS.md says two
   versions share the store; a fresh old process cannot), and add a test
   that a successor killed after `join`'s flip leaves `current` on the
   release that claims.

6. **Two workers of one release can each start a successor.**
   successor.rs:113 reads the live set, 118 decides from it, then 119-121
   awaits `Supervisor::stop` (up to `STOP_SETTLE` plus ten seconds of
   grace) before `spawn` (122) and the registration (124); nothing
   re-checks after the await and nothing is held across it. Input: two
   daemon workers of the same release live at once (a hand-started
   `forge work` beside the unit, which nothing prevents) and a staged
   release. What goes wrong: two successors of one release, which never
   supersede each other (`w.version != self.version`, 116), both claim,
   both send `MAINPID=` (74), and one of them is not the unit's main pid.
   `claim` is an atomic `UPDATE ... WHERE state='queued'`
   (store/tasks.rs:707-713), so a task does not run twice; the rest of
   the handoff's guarantees are gone. *Task:* take an exclusive `flock` on a
   file beside `bin/staged` across the check, the plugin stop and the
   spawn-and-register, or register a claiming row before stopping the
   plugins, and re-run `live_workers` after the stop.

7. **Contract migrations are applied at one moment only, which a chain
   of successors never reaches.** `apply_contracts` runs once, in `join`
   (successor.rs:76), and returns 0 when any live worker is on another
   version (store/mod.rs:729-735); the predecessor is alive at exactly
   that moment, by construction. Nothing calls it again when the
   predecessor exits (grep: only successor.rs:76 and the fresh-database
   path, store/mod.rs:379). Every deploy that goes through a successor
   therefore leaves its contract steps pending until something else
   restarts the worker with no other version alive. Input: any release
   with a `-- contract` migration, deployed through the successor path.
   *Task:* have the worker's pass call `apply_contracts` when it is not
   superseded (it is one `SELECT` when everything is applied), so the
   step runs on the first pass after the last older worker is gone; add
   a store test with two registered versions and one exit.

8. **The drain keeps ticking.** `tick_run_workflows`, `schedule_tick`
   and `event_tick` (worker.rs:944-946) run on every pass before
   `superseded` is computed (947) and are not gated on `stopping` or
   `superseded`; so a draining worker, for up to the drain's 40 minutes,
   resolves workflows with its own older code and queues scheduled and
   event-triggered jobs (`job::start_scheduled`, job.rs:638;
   `start_event`, job.rs:765) beside the successor doing the same, and
   moves the shared per-workflow event cursor (worker.rs:643-647) past
   events "whatever its type". The docs promise claims go to the newest
   version only (OPS.md); triggers are not claims and both versions fire
   them. The two ticks racing on one event (`start_event` checks
   `job_for_trigger` and then inserts, job.rs:780-796, without the
   unique-index handling `start_webhook` has) log a spurious `event tick:
   UNIQUE constraint failed`. *Task:* compute `superseded` first and run
   the three ticks only when the worker is neither superseded nor
   stopping; give `start_event` the same unique-conflict handling as
   `start_webhook`.

9. **One tick error skips the successor check and the claim.** The pass
   is a single `async` block whose ticks use `?` (worker.rs:944-946), so
   an `Err` from `event_tick` returns before `superseded` (947) and the
   claim loop (957). `event_tick`'s reads go through `report::log::read`,
   whose `read_line` (log.rs:152) fails on a line that is not UTF-8;
   the log is written by Rust only, so the reachable input is a torn
   `write_all` (a full disk) or a hand edit, and the failure repeats on
   every pass because the cursor never moves. The worker logs "worker
   pass failed; retrying" every ten seconds, claims nothing and never
   starts a successor. (By reading; not reproduced.) *Task:* run each
   tick as its own step whose error is logged and does not end the pass
   (as `schedule_tick` already does per workflow), and make `read_file`
   skip a line that is not UTF-8 the way `forge events` skips one that
   is not JSON.

### 6.2 Plugin supervision

10. **A plugin has no owner beyond the worker's life, and nothing
    collects what the worker leaves.** Three paths, all confirmed. (a)
    The double-signal abort: `requeue_aborted(...)?` (worker.rs:1101)
    returns from `work` before `plugins.stop()` (1108) and
    `succession.leave` (1111); the runtime then drops the supervising
    tasks and `kill_on_drop` SIGKILLs each leader, but the rest of its
    group (a shell plugin's `forge events --follow`) is not the `Child`
    and survives. (b) A worker killed while it is stopping plugins:
    `Supervisor::stop` takes up to 400 ms plus ten seconds per stubborn
    plugin (plugins.rs:323, 333, 478-496); the e2e `Worker` guard
    SIGTERMs, waits five seconds and SIGKILLs the worker
    (tests/e2e/support.rs:71-96), so a plugin that answers SIGTERM slowly
    is left running and never gets its SIGKILL. (c) SIGKILL or a crash
    outside systemd (a hand-run or test-run worker): no parent-death
    signal is set (`client/src/lib.rs:489` sets one for its own
    subscription; `spawn_plugin` does not), the group is left, and
    `Supervisor::start` does not look for it: `plugins-run/<name>.json`
    still says `Running { pid }` (plugins.rs:341-345) and no one reads it
    back. These are the shapes that leave the 118 processes a run of the
    plugin e2e tests accumulates (worker killed by `Reap`,
    tests/e2e/successor.rs, or by `child.kill()`), each one a `sh` looping
    on `forge events --follow` until something kills it. *Task:* at `Supervisor::start`, read
    each `plugins-run/<name>.json`; for a `Running { pid }` whose group
    still exists and whose start identity (`store::start_of`) matches the
    one recorded with it, SIGTERM then SIGKILL the group before the lock
    is taken; record the start identity in the state file; make the
    abort path stop the supervisor before returning; and set
    `PR_SET_PDEATHSIG` on the leader in a `pre_exec` (from the thread
    that spawns, which is a runtime worker for the life of the runtime).

11. **The per-plugin lock does not follow the plugin, so an orphan and
    its replacement run together.** `try_lock_plugin` (plugins.rs:399-409)
    opens the lock file with std's default `O_CLOEXEC` and holds it in the
    worker; docs/PLUGINS.md (181-183) and the comment at plugins.rs:395
    say it is held "for as long as the plugin's process lives". When the
    worker dies without stopping its plugins (finding 10) the lock is
    released with it, the next worker takes it and starts a second copy
    beside the survivor: two notify plugins send every message twice,
    two signal plugins both poll the inbound side. *Task:* let the
    plugin's group inherit the locked file description (clear
    `FD_CLOEXEC` on it in a `pre_exec`), so the flock lives exactly as
    long as any member of the group and a second copy cannot start while
    one member survives; test it by killing a worker with SIGKILL and
    asserting the next supervisor does not start the plugin until the
    group is gone.

12. **The group is only signalled while the leader is alive.** On
    SIGTERM `stop_child` (plugins.rs:478-496) escalates to SIGKILL only
    in the timeout branch; a leader that exits inside the grace (a
    shell's default SIGTERM) returns at 484 with a group member that
    ignored or is slowly handling SIGTERM still running, unowned, and the
    lock (a `Supervised` field) released once the task returns. The other
    exit, the leader ending by itself, never signals the group at all
    (supervise_plugin, plugins.rs:593-651): `statusline.sh` backgrounds
    `forge events --follow` and a reader (`statusline.sh:97-105`) and
    relies on a `trap` (113-114) to kill them, so a leader that dies by
    SIGKILL, or between starting them and installing the trap, leaves both, and `restart`
    starts a new leader (and new followers) beside them at up to one per
    backoff. *Task:* after the leader has been reaped, on both paths,
    `kill(-pgid, SIGKILL)` the group (an `ESRCH` is the normal answer),
    with a comment on the pid-reuse window this leaves and how it is
    closed (do it before the leader is reaped with `waitid(WNOWAIT)`, or
    keep the leader's pid unreaped until the sweep is done); test with a
    plugin whose leader exits and whose child `sleep 1000`s.

13. **`enabled_plugins_now` turns "cannot read" into "nothing is
    enabled".** plugins.rs:658-660 and 662-664 return an empty map when
    `config::load_home` or `enabled_plugins()` fails, and the reconciler
    treats absence as "disabled" (`gone`, 758-767): every running plugin
    is stopped (SIGTERM, up to ten seconds, cursors mid-flight) and
    restarted ten seconds later when the file parses again. Input: any
    tick that lands while `config.toml` is being edited (an invalid
    file), or while a plugin's `plugin.toml` is being rewritten (the
    catalog reports a problem, so the plugin is not in `cat.plugins`,
    667). The worker's own reload keeps the previous config when a new
    one does not validate (reload.rs:1-9, OPS.md "Config reloads between
    claims"); the supervisor re-reads the file from disk and bypasses
    that. *Task:* on an error from either read, keep the current running
    set and log once; distinguish a plugin the catalog lists as broken
    from one that is absent; and use the config the worker has already
    validated (`reload.rs`) rather than the file.

14. **`forge plugin uninstall` does not stop the plugin it removes.**
    cli/deploy.rs:346-351 clears the flag and runs `remove_installed`
    (plugins.rs:305-311, `remove_dir_all`) at once; the running plugin
    keeps its deleted working directory for up to `RECONCILE_SECS`
    (plugins.rs:326), and a plugin that exits in that window with
    `restart = always` is respawned by `supervise_plugin` into a
    directory that no longer exists (a "failed to start" state, retried
    with backoff, until the tick stops it). The command's help says "Stop
    a plugin, clear its enabled flag, and remove the installed copy".
    *Task:* wait, bounded, for the run state to leave `running` (or ask
    the reconciler through the same request file `restart` uses) before
    removing the directory.

### 6.3 The store, the CLI a plugin calls, and the event subscription

15. **Every `forge` call takes the database's write lock, and a release
    older than the schema refuses to open it.** `migrate`
    (store/mod.rs:778-807) starts `BEGIN IMMEDIATE` (779) on every
    `Store::open`, before it has read `user_version`, so a plugin's
    read-only `forge show`, `forge snapshot` or `forge message record`
    waits behind, and blocks, every other writer, and while a successor
    is running a long migration (the data steps run inside that
    transaction, 793-798) each such call waits up to `busy_timeout`
    (60 s, store/mod.rs:364) and then fails with "database is locked";
    the plugins call the CLI with `2>/dev/null` (`signal.sh:310, 403`) and
    carry on, so the failure is a dropped notification or a message not
    recorded. Between the successor's `Store::open` and its flip
    (finding 5) every `forge` call through `current` is the old binary,
    which errors at 782. *Task:* read `user_version` under a deferred
    transaction first and take `BEGIN IMMEDIATE` only when it is below
    the target (re-reading inside), so the common open is read-only; add
    a store test with a held writer that asserts a current-version
    `Store::open` returns without waiting on it.

16. **`notify.sh` and `github-issues.sh`, on a failed first `forge
    snapshot` or an empty cursor file, exit 0 and are never restarted.**
    `notify.sh:27-42` and `github-issues.sh:85-94` take their start offset
    from `forge snapshot | sed` when no cursor file exists, or from
    `cat cursor`; a `snapshot` that fails (store locked past its timeout,
    finding 15; a release skew) or a cursor file truncated by a kill
    between `>` and the write (`notify.sh:73`) gives an empty offset,
    `forge events --since ""` fails to parse it (`Cursor::from_str`,
    log.rs:21-25) and exits non-zero, but the script's status is the
    `while` at the end of the pipeline, which is 0 (no `pipefail`, `set
    -u` only). `restart = "on-failure"` (the default, plugins.rs:621) does
    not restart a zero exit, so the plugin is `stopped: exit 0` until the
    worker restarts. Confirmed by running both scripts against a stub
    `forge` whose `snapshot` and `events` fail. The other two scripts that
    read a cursor are not this defect. `signal.sh:322-331` takes its offset
    the same way, but it runs `outbound` and `inbound` as background loops
    and ends with `while kill -0 ...; done; log "a loop exited; stopping so
    the supervisor restarts both"; exit 1` (545-551), so a dead loop is a
    non-zero exit and the supervisor restarts it on the backoff schedule;
    with a persistently failing `snapshot` that is a restart loop, not a
    silent stop. `statusline.sh:80-105` uses a fifo and a background
    `events`, so a failed `events` does not end it either: its heartbeat
    loop (118) keeps running and the plugin stays running without following
    events, which is a different defect from an exit 0 and is not
    recorded here. *Task:* in `notify.sh` and `github-issues.sh`, exit
    non-zero when the offset is empty or the `events` process fails (`set
    -o pipefail` where the shell allows it, else check `$?` through a
    fifo as `statusline.sh` does), write the cursor through a temporary
    file and `mv`, and add the case to `tests/e2e/event_cursors.rs`.
    Leave `signal.sh` out of the exit-status change.

17. **The subscription replays, or drops, depending on the plugin, and
    the one signal the CLI gives to say so is ignored (the 249-message
    replay, task 820).** `report::log::read` (log.rs:126-137) answers a
    cursor whose generation is not the current one, or is not the
    immediately previous one with its tail present, or whose offset is
    past the file, with `resync` and then reads the whole current
    generation from its start; docs/CLIENT.md (1302-1305) tells
    consumers to refresh their snapshot on `resync`, and none of the
    four reference plugins mentions it (`grep resync plugins/` is empty).
    So a plugin stopped across two rotations, or whose cursor was made
    for another log, delivers a generation (up to 50 MB,
    `EVENT_LOG_SIZE_LIMIT`, report.rs:13) of `task_done` events as new
    notifications; and with finding 1 a plugin can be down for hours,
    after which the ordinary catch-up (correctly gap-free) also arrives
    as a burst of stale "task N failed" messages, since nothing looks at
    the event's `ts`. The ordering differs by plugin as well: `signal.sh`
    writes its cursor before it handles the event (333), so a plugin
    killed mid-handler loses that notification, while `notify.sh:73` and
    `github-issues.sh:120` write it after, so the same kill sends it
    twice. I could not recompute the 249 from here (it needs that
    machine's `events.jsonl` and cursor); the mechanism is confirmed, the
    count is not. *Task:* teach the four plugins the three cases: on a
    `resync` event reset the cursor to `forge snapshot`'s
    `events_offset` and drop the batch; skip events whose `ts` is older
    than a per-plugin bound (default one hour) unless configured
    otherwise; and pick one cursor order (after handling, with the
    handlers idempotent) for all four, documented in docs/PLUGINS.md.

18. **`forge doctor` cannot see any of this.** `check_plugins`
    (doctor.rs:506-540) is `ok` whenever the catalog has no problem,
    whatever the enabled plugins' run states say; `read_run_state`
    (plugins.rs:370-375) is the last file written, so after a worker
    crash a dead plugin still reads `running pid N, up …` forever
    (`RunState::Running` is never checked with `pid_alive`), and after
    finding 1 an enabled plugin reads `stopped: stopped by worker` under
    a green row. *Task:* warn for an enabled plugin whose state is
    `Stopped` while a worker is live, or whose `Running` pid is not
    alive; add both to the doctor tests.

19. **Smaller: fixed temporary names and blocking calls in async code.**
    `release::point` (release.rs:88-91) and `install` (71-72) use one
    temporary name per pointer or release id and delete it first, so two
    flips (the successor's `take_over` and a deploy's fallback flip)
    can fail with `EEXIST`, and two installs of one id delete each
    other's half-copied directory; `request_restart` (plugins.rs:437)
    does the same with `<name>.restart.tmp`. `stop_requested` runs
    `systemctl --user is-active` with no timeout on the runtime thread on
    every pass (successor.rs:194, 277-286), `leave` sleeps 200 ms at a
    time for up to 120 s on it (209-214), and `enabled_plugins_now` does
    file and SQLite work, with the connection's mutex held and
    `retry`'s `thread::sleep` (store/retry.rs:15), on the reconciler's
    task (plugins.rs:657-669): a hung user bus or a locked store stops
    signal handling for as long as it lasts. *Task:* unique temporary
    names (the pid and a counter), a bounded `systemctl` (a timeout and
    `kill_on_drop`), and `spawn_blocking` for the reconciler's reads.

### 6.4 Read and found sound

- **Stopping a live plugin's group** (plugins.rs:478-496, 505-532): its
  own process group, SIGTERM to `-pid`, SIGKILL after ten seconds, all
  plugins signalled before any is waited on (`Supervisor::stop`, 799;
  the unit tests at plugins.rs:1015-1157 hold this), and the reconciler
  draining what a racing restart spawned (777-791). The gaps are in
  findings 10-12, outside this path.
- **The restart generation** (plugins.rs:415-441, 733-757): a counter, not
  a timestamp; the generation is read at 751 before the process is
  spawned (754), so a request that lands during a stop is either seen by
  the next tick or already covered by the process that starts after
  it. Two concurrent `forge plugin restart` calls coalesce into one
  restart, which is what either wanted; only the shared temporary name
  (finding 19) can make one of them fail. `disable` then `enable` inside
  one 10-second tick is never seen and restarts nothing; documented
  (plugins.rs:426-430), `restart` is the verb.
- **Single supervision on the clean path**: the lock is held by
  `Supervised` until its task ends (681-701), `stop` signals every
  plugin before it waits (697-700), and a supervisor that finds the lock
  held skips and retries (747-757); the handoff order (stop, then
  spawn) in successor.rs:119-122 is right, and the tests at
  plugins.rs:1163-1189 hold it. The failure is when the holder dies
  (finding 11).
- **The `MAINPID` handover itself** (successor.rs:59-92, 200-229, 308-331):
  `Type=notify` with `NotifyAccess=all`, the successor is a child in the
  unit's cgroup, `READY=1` and `MAINPID=` go on the inherited
  `NOTIFY_SOCKET` (abstract sockets included), the capability file is
  written by rename, and `leave` waits for it (bounded at 120 s) before
  the old main pid exits. What was found is around it (findings 3-5), not
  in the datagram.
- **`pid_alive`** (worker.rs:40-52): kill 0, `EPERM` counted as alive,
  non-positive and out-of-range pids refused. The gap is the row it is
  applied to (finding 2).
- **Release layout** (release.rs:27-145): symlinks flipped by `rename`,
  `previous` kept, `flip` a no-op on the live id, `restore` symmetric,
  `install` through a temporary directory and never touching an existing
  release, `running()` resolving `FORGE_RELEASE`, then the executable's
  directory, then `current`. `binary::through_current` gives a plugin a
  `FORGE_BIN` that survives a flip (binary.rs:11-26).
- **The event log's own I/O** (report/log.rs): one lock for readers and
  writers, generation header written through `pending` and recovered
  before the next append, torn tails not consumed, a bounded read
  budget; the unit tests at log.rs:176-284 cover rotation, a lost
  archive and a torn tail. The problem is what the consumers do with
  `resync` (finding 17), not the log.
- **Migrations for two versions** (store/mod.rs:778-807, migrations.rs:
  720-726): additive steps only on the ladder, a contract step tagged and
  deferred until no older worker is alive, a test that parses every
  migration, `BEGIN IMMEDIATE` so two openers do not both migrate. The
  gaps are when a contract step is finally applied (finding 7) and the
  lock's cost on every open (finding 15).

### 6.5 The plan

Ordered by what it stops: **stage 0** is findings 1, 3 and 4 (one change
to `Succession::superseded`: restart the supervisor, close the reaped
row, do not claim after a clean exit, do not start a successor while
stopping), which is what turned a failed handoff into a quiet evening;
**stage 1** is finding 5 (move the flip after the successor has proved
itself, or undo it), the only one that can leave the unit restarting a
broken release; **stage 2** is findings 10-12 (ownership of the group and
of the lock), after which the e2e suite stops leaving processes behind;
**stage 3** is findings 7-9, 13 and 15 (the pass structure, contracts and
the write lock); **stage 4** is 16-18 (the plugins' own loops and
doctor); findings 2, 6, 14 and 19 ride with whichever stage touches the
same function.

# The edges, read cold: section 3, deploy and the release layout (2026-09-27)

The kernel's edges (the sandbox and egress proxy; plugin supervision and
the successor handoff; deploy and the release layout) were read cold, one
task each, and only what was confirmed at a line is recorded, with the
input that reaches that line and what goes wrong there. Suspicions are
not recorded. Each defect ends with a paragraph a follow-up task can be
filed from as written. This part is section 3. Sections 1 and 2 are
written by their own tasks and are not in this tree; the closing table
carries section 3's rows and says where theirs go.

Unless a defect says *reproduced*, it was confirmed by reading and
tracing the code, not by running it: no systemd user session and no
`cargo build --release --workspace` ran for this read. Two claims were
run and are marked.

## 3. Deploy and the release layout

Read as one system: `src/deploy.rs` (675 lines), `src/deploy_look.rs`
(261), `src/release.rs` (192), `src/init.rs` (417, `--relink` lives here),
the `deploy-self` and `deploy-user-service` builtin operations (196 and
59 lines, `deploy-command` beside them for comparison), and doctor's
release, worker, shadowing and plugins rows (`src/doctor.rs`,
`src/workflows/shadow.rs`). Followed into: `src/successor.rs` and
`worker.rs`'s loop (the handoff `deploy-self` now hands its result to),
`src/upgrade.rs` (the other writer of `current`), `src/unit_path.rs`,
`src/store/deploys.rs`, `src/operation.rs` and `src/checks.rs` (how a
method is run and timed out), `src/git.rs` (`stage`, `kernel_repository`,
`fresh_archive`), `src/workflows.rs`'s catalog loader, `src/queue.rs`'s
`answer`, `src/landing/effects.rs`, `build.rs`, `deploy/forge-worker.service`,
and docs/DEPLOY.md and docs/OPS.md for what each promises.

### 3.1 Defects confirmed while reading

Ordered by what they cost. Numbers are stable: the closing table uses
them as `E3-n`.

1. **A successor that dies, or is slow, takes the worker unit down with
   it, and the staged release is tried again after every restart until
   systemd gives up.**
   `Succession::superseded` returns `true` the moment it has spawned the
   successor (successor.rs:122-130). An idle old worker then leaves its
   loop (worker.rs:1020-1023) and `leave` waits up to `HANDOVER_WAIT`,
   120 s, for `bin/successor-capable` to name the child (successor.rs:
   200-229). If the child exits first, or has not written it in 120 s,
   `leave` returns `Err`, `work` returns it (worker.rs:1119-1122) and the
   process exits non-zero. The unit `forge init` writes is
   `Restart=on-failure`, `StartLimitBurst=5` in `StartLimitIntervalSec=
   10800` (init.rs:69-70, 81). The restarted worker is on `current`, the
   old release, `staged` still names the release that just failed (nothing
   clears it, defect 3), and `failed` is an in-memory set
   (successor.rs:53) that the restart emptied, so it spawns the same
   successor, which dies the same way. The fifth restart in three hours
   trips the start limit: the unit is `failed` and no worker runs.
   *Input:* any staged release whose `forge work` exits before it writes
   the capability file. The scratch doctor does not catch that (defect
   6): a migration that fails on the live `forge.db` fails in
   `Forge::open`, before `join`. The slow case needs no crash: a
   successor still migrating a large `forge.db` at 120 s makes the old
   worker, still the unit's main pid (`MAINPID=` is sent from `join`,
   successor.rs:74), exit non-zero, which ends the service and takes the
   rest of its cgroup, the successor, with it (systemd's behaviour for a
   main process that exits; not run here). A related hole: the
   successor flips `current` in `join` (successor.rs:72, 291-306)
   before it has proven anything, and the unit's `ExecStart` runs
   `current/forge` (init.rs:77). A successor that crashes after `join`
   leaves `current` on the broken release, and every restart runs it.
   Nothing ever flips back to `previous`.
   *Task:* "A successor's failure must not cost the worker. In
   `Succession::leave` (successor.rs:200-229) treat a successor that
   exited or has not taken over as a handover failure, not a worker
   failure: exit 0 after logging it, so `Restart=on-failure` does not
   fire; make the wait cover a migration (poll the child's liveness and
   `workers` row, not a fixed 120 s). Persist a failed release
   (`bin/staged-failed`, the sha and the time) so a restarted worker
   does not spawn it again, and clear or rename `staged` when it does.
   Have the successor flip `current` only after its store is open, its
   `MAINPID=` sent and its first pass done, and have the worker that
   started a successor which then dies put `current` back to the release
   it runs. Tests: a fake release whose `forge` exits 1 immediately
   leaves the old worker running, `staged` retired and one restart, not
   five; a successor that sleeps past the old wait is not killed."

2. **A worker whose successor dies while it still holds tasks stops its
   plugins and never starts them again.**
   Before spawning, `superseded` stops this worker's plugins so their
   locks pass to the successor (successor.rs:118-121). Only the spawn
   error branch restarts them (successor.rs:135). When the child later
   exits, the branch at successor.rs:106-112 marks the release failed and
   the function falls through to `return Ok(false)` (successor.rs:139),
   so the worker claims again with `plugins` still `None`. The channel
   plugins are then dead for the life of that
   worker.
   *Input:* a staged release whose worker exits while the old one is
   still running an attempt (long attempts are the norm; `TimeoutStopSec`
   is 2400 s). *Task:* "When `superseded` sees its successor exit
   (successor.rs:106-112), restart the supervisor it stopped:
   `*plugins = Some(Supervisor::start(f.clone()))`, as the spawn-error
   branch does. Test: a successor binary that exits at once; after the
   next `superseded` the plugins row shows the plugins running under the
   old worker."

3. **`staged` is never consumed, so `forge upgrade` and a manual
   rollback are undone by the next worker start.**
   `staged_successor` returns `staged` whenever it names a runnable
   release other than the one this worker runs and no live worker is on
   it (successor.rs:149-158). Nothing removes or updates `staged` after
   a handover; only `deploy-self` writes it. So after any self-deploy
   `staged` names the last deployed release for good. Then `forge
   upgrade` flips `current` to `releases/<version>`, and
   `bring_up` restarts the worker (upgrade.rs:347-366, 300-310): the new
   worker's version is `<version>` (its executable sits in that
   directory, release.rs:47-51), `staged` is the older deploy, so it
   starts a successor on the older release, which flips `current` back
   (successor.rs:291-306) and restarts web and portal on it. The upgrade
   is reversed within a poll interval, with no message beyond a line in
   the worker log. An operator's emergency `ln -sfn` of `current` to
   `previous`, followed by `systemctl --user restart forge-worker`, is
   reversed the same way.
   *Input:* `forge upgrade <tarball>` (or a hand flip of `current`) on a
   machine that has done one self-deploy. *Task:* "Make `staged` a
   request that is acknowledged. When a successor takes over
   (`take_over`, successor.rs:291) or a worker finds `staged` equal to
   the release it runs, remove `staged`; have `forge upgrade`
   (upgrade.rs, before the flip) and `release::flip` callers other than
   deploy-self drop `staged`, and have `staged_successor` ignore a
   `staged` older than `current` in `previous`'s lineage. Doctor's worker
   row should FAIL when `staged` names a release that is not `current`
   and no successor is starting. Test: upgrade with a stale `staged`
   leaves `current` on the upgraded release after the worker restarts."

4. **A failing `deploy-self` writes its stale copy of the pointers over
   whatever a successor has flipped since, and restarts web and portal
   for a build error.**
   The script reads `current`, `previous` and `staged` once, after it
   takes the lock (deploy-self.toml:47-49), then builds for minutes.
   Any failure with `armed=1` runs `restore`, which rewrites all three
   pointers from those values unconditionally (60-71) and restarts
   `$units` (68-70). The second landing of two back to back is the
   common trigger: it waits on the lock while the first builds, reads the
   pointers the instant the first exits (before the first's successor
   has started, one poll interval later), and its build then outlasts the
   successor's `take_over`. If that build fails: the successor has
   flipped `current` to the first release and restarted web and portal;
   `restore` sets `current` back to the release from before, `staged`
   to the first release, and restarts web and portal onto the old
   binary. The worker runs the first release, web and portal run the old
   one, `current` names the old one, and the next unit restart brings
   the worker up on it. Separately, any failure restarts `$units` even
   when nothing was flipped (a `cargo build` error, a failed doctor):
   healthy web and portal are bounced for nothing, and the restart
   is only worth doing after a flip.
   *Input:* two landings on the Forge repository within a build time of
   each other, the second failing to build. *Task:* "Make `restore` in
   deploy-self.toml touch only what this run wrote, and only if it is
   still what this run wrote: write each pointer this run changes to a
   variable, and in `restore` put a pointer back only when `readlink`
   still equals the value this run set (compare-and-set); track `flipped`
   and restart `$units` only when `current` was flipped by this run. Do
   not read `was_*` from before the lock wait. Test: a script run whose
   build fails while a fake successor flips `current` mid-build leaves
   `current` on the successor's release."

5. **`forge deploy forge self` reports success when it has only staged a
   release; nothing checks that the release went live.**
   In the default, successor-capable case the script exits 0 right after
   `staged` is written (deploy-self.toml:136-140). `deploy::run` then
   records `check_ok: true` and emits `DeployFinished { ok: true }`
   (deploy.rs:358-382), and `forge deploy` exits 0; `forge deploy log`
   prints `ok`. What is live is unchanged: `current` is the old release
   and will not move until the worker's next poll, if a successor starts
   at all (defect 1), and the takeover's web and portal restart is
   `--no-block` and unchecked (successor.rs:299-305). The target's smoke
   step and the deploy look (deploy.rs:300-356) run against the old web
   client. The default check that would say whether the new release
   serves, the `/tasks` fetch (deploy-self.toml:169-177), is only reached
   in the legacy branch. The row then also becomes the rollback target
   for the next failed deploy (deploy.rs:387-391), a release that may
   never have been live.
   *Input:* any self-deploy under a worker that starts successors.
   *Task:* "Split 'staged' from 'live' in `deploy::run` for `deploy-self`.
   After the method returns with the worker successor-capable, wait,
   bounded, for the `workers` table to show a live worker on the deployed
   sha and for `current` to name it, then run the target's check (the
   web `/tasks` fetch) and the smoke step against that. Fail the deploy,
   with the reason 'staged but never became live', if the wait times out,
   and put `staged` back to what `current` names so it is not retried.
   The deploy row's output should say 'staged' and 'live' as separate
   lines. This runs inside the landing's slot, and the successor is
   started by the main loop, so it cannot deadlock. Test with a fake
   worker that never takes over: the deploy fails and exits 1."

6. **The scratch doctor proves the schema on an empty store, and its
   pass condition is a substring of JSON.**
   deploy-self.toml:112-119 runs the new release's `forge doctor --json`
   against `mktemp -d` and passes on `"name":"schema","status":"ok"`
   after deleting spaces and newlines. The check is `PRAGMA
   user_version` on a store the binary just created (doctor.rs:452-457),
   so it proves the migration ladder runs on nothing. A migration that
   fails on real data, or a new binary that cannot open the live store,
   still passes and fails later in the successor (defect 1). The match
   also depends on serde's field order in `Check` (doctor.rs:28-33),
   which docs say is not a stable contract (doctor.rs:20-27).
   `forge upgrade` does it the right way round (`backup_store` then the
   new doctor against the real home, upgrade.rs:186-206, 316-343).
   *Task:* "Have `deploy-self` copy the live `forge.db` with `sqlite3
   .backup` (as `upgrade::backup_store` does) into the scratch home
   before the new binary's doctor runs, and require `schema` `ok` there;
   parse the JSON with the new binary itself (a `forge doctor --json
   --only schema` that exits non-zero) instead of grepping. Test: a
   release whose migration 'N+1' fails on a store with a row in it is
   refused at staging."

7. **A catalog copy of a kernel-coupled operation shadows the built-in
   without notice, and doctor calls it fine.**
   `deploy::run` resolves the method through the catalog
   (deploy.rs:251, operation.rs:445-456) and hands it `FORGE_WORKER_
   SUCCESSORS` (deploy.rs:43-49), a contract between this binary and
   the script text. A file in `<home>/workflows/actions/deploy-self.toml`
   wins over the built-in unless `has_operator_commit` calls it a
   stale seed, which it does by author and subject only: any commit on
   the file by an author other than `forge`, or whose subject does not
   start `catalog: built-in`, makes it an operator edit
   (shadow.rs:83-95). An operator edit is loaded and is not a fault:
   `doctor_check` is `Warn` only for stale seeds (shadow.rs:329-341), so
   for an edit it prints `deploy-self.toml (operator edit, 1d old, 40
   diff line(s))` with status ok, and nothing at deploy time says which
   text ran. A day-old copy from before the successor handoff therefore
   ignores `FORGE_WORKER_SUCCESSORS`, restarts the unit itself and races
   the successor, on every self-deploy, with every check green. (I could
   not inspect the operator's catalog for how that copy came to be
   classified as an edit; the classification rule is the only path
   here.)
   *Input:* a catalog copy of `deploy-self` (or `deploy-smoke`,
   `deploy-static`) committed by anyone but Forge, older than the
   binary. *Task:* "Never resolve `deploy-self` from the catalog:
   `deploy::run` should take its text from `BUILTIN_OPERATIONS` for
   `SELF_METHOD`, and say in a `Note` when any other deploy method came
   from a catalog copy with its diff line count. Make doctor's
   shadowing row `Warn` for an operator edit that shadows a built-in
   whose text changed after the copy's last commit (the copy's blob
   is not any historical built-in and the built-in's hash is newer),
   and `Fail` for a shadow of `deploy-self`. Test: a catalog with a
   copy of deploy-self, committed by someone other than Forge, is not
   what runs, and the release is staged, not restarted."

8. **The stale-seed rule reads git history, so an edit not yet committed
   (or committed by `forge init`) is 'a stale seed', ignored, and then
   deleted without a trace.**
   `has_operator_commit` is false for a file with no commits
   (untracked) and for one whose only commits are authored `Forge`
   (shadow.rs:83-95). `forge init` runs `commit_all` (init.rs:355,
   git.rs:691-703): `git add -A` then a commit authored `Forge`, so
   whatever the operator had left uncommitted in the catalog, including
   an edited `actions/*.toml`, is committed as a seed. From then on the
   loader skips the file (workflows.rs:1470-1476) and the built-in
   applies: the operator's edit silently stops working. `forge
   workflows refresh` then removes stale seeds (cli/workflows.rs:590-
   594): for an untracked or uncommitted file `shadow::remove` is a
   plain `remove_file` with no commit (shadow.rs:279-298), and the edit
   is gone from disk and from history.
   *Input:* `cp` a built-in into `workflows/actions/`, edit it, run
   `forge init` (or just `forge workflows refresh`). *Task:* "Classify
   by content, not by who committed it: a copy is a stale seed only if
   its blob equals a built-in text this or an earlier release shipped
   (keep the list of historical hashes next to `BUILTIN_*`); anything
   else, committed or not, is an operator edit. `forge init` must not
   `commit_all` the catalog; commit only files it wrote. `shadow::remove`
   must refuse an untracked or dirty file. Tests: an uncommitted edit
   survives `refresh` and is reported as an edit; a copy equal to an old
   built-in is a seed whoever committed it."

9. **The deploy question is one `forge answer` refuses, and asking it
   flips a landed task to blocked.**
   `ask` picks the project's newest terminal task on the repository,
   including `Succeeded`, sets it `Blocked` with the failure text and
   `update_task`s it whole (deploy.rs:179-221), or files a no-work
   `direct` task in that state when none exists. But
   `queue::answer` only answers a task whose last agent attempt is
   `NeedsInput` (queue.rs:1252-1262). A landed task's last attempt
   succeeded; the filed task has no attempts. So `forge answer` on the
   question docs/DEPLOY.md ends every failed deploy at fails with 'not
   blocked on a question'. And landing/effects.rs:41-43 promises "a
   deploy's own failure never changes the task's landed state", which
   `ask` does: the task that landed is now `blocked`.
   *Input:* any deploy check failing after a landing, or `forge deploy`
   by hand on a project whose last task succeeded. *Task:* "`deploy::ask`
   must not edit an existing task. File a new no-work task, with its
   own `question_to` and a `deploy_id`, and let `queue::answer` (and
   the web client's answer verb) close a deploy question: record the
   decision and move the task to `withdrawn` without re-running
   anything. Tests: after a rolled-back deploy the landed task is still
   `succeeded`, and `forge answer <question-id> 'ok'` succeeds."

10. **An error after the deploy has started skips both the rollback and
    the question, and can overwrite the recorded outcome.**
    Everything inside `run`'s async block that uses `?` ends in
    `record_deploy_error` (deploy.rs:483), which finishes the row as a
    failure and returns the error, and nothing else. The smoke step's
    `?` (deploy.rs:304, `run_deploy_smoke` fails when it cannot create
    its output directory) fires after the method has succeeded: the
    new commit stays deployed, the row says failed, no rollback runs,
    nobody is asked. The rollback's own `deploy_at(...).await?`
    (deploy.rs:429-438) does the same when `previous.sha` cannot be
    archived: for an operator-run `forge deploy`, `src` is the
    registered checkout (deploy.rs:146-154), which lacks a sha an
    on-landing deploy staged only in the kernel repository (the case
    deploy.rs:124-130 describes). The failed deploy stays live, the row
    has `rolled_back_to: None`, no question. And `ask(...)?` at
    deploy.rs:420-425 and 479 comes after `finish_deploy` recorded the
    rollback: an error there reaches `record_deploy_error`, whose
    `finish_deploy` is unconditional (store/deploys.rs:310-312) and
    overwrites `rolled_back_to` with `None` and the reason with the
    error text.
    *Input:* `forge deploy demo prod` where the previous passing deploy
    was on-landing. *Task:* "Resolve the rollback's source the way an
    on-landing deploy does: archive `previous.sha` from the kernel
    repository when the registered checkout lacks it. Turn a smoke-run
    error into a failed smoke result (`sr.ok = false`, the error as its
    tail) so the normal rollback and question run. Make
    `record_deploy_error` refuse to overwrite a row that already has
    `finished_at` (`WHERE id=? AND finished_at IS NULL`), and file the
    question from `record_deploy_error` when a rollback was not
    attempted. Tests for each of the three."

11. **The 'older than live' guard fails open, runs before the lock, and
    looks only at `current`.**
    `origin_truth` refuses a commit that is an ancestor of the live
    release (deploy.rs:107-120), which is what keeps migrations from
    running backwards. Three holes. (a) Every step that can fail is
    swallowed into 'allowed': the live id is read as a commit with `if
    let Ok(..) = rev_parse(..)`, and `is_ancestor` returns `false` for
    any git error (git.rs:522-527). A live release whose id is not a
    commit the kernel repository has (`forge upgrade`'s release is
    named `0.4.0`; a `--relink` id is a short sha) skips the guard
    silently. (b) It compares with `current`, not with `staged`: between
    a stage and the successor's flip a deploy of an older commit passes.
    (c) It runs at deploy.rs:262-263, outside `.deploy-self.lock`, which
    the script takes later; the lock is not fair, so of two waiting
    deploys the older commit can stage last, and the guard ran before
    either.
    *Input:* on-landing deploys of two landings whose deploys reach the
    lock out of order, or `forge deploy forge self --sha <old>` while a
    newer release is staged. *Task:* "Take `bin/.deploy-self.lock`
    in `deploy::run` for `SELF_METHOD` around `origin_truth` and the
    method (pass `FORGE_DEPLOY_LOCK_HELD=1` so the script skips its own
    flock, which would otherwise wait on its parent), compare against
    the newer of `current` and `staged`, and fail closed: a live or
    staged id that does not resolve to a commit is refused unless
    `--force`, saying so. Tests: with `staged` naming a descendant, an
    ancestor is refused; a live id of `0.4.0` refuses rather than
    skips."

12. **Nothing serialises deploys of any method but `deploy-self`, and the
    rollback target can be the commit that just failed.**
    `deploy::run` takes no lock (deploy.rs:238-484). The other methods
    (`deploy-static`, `deploy-command`, `deploy-user-service`) `rsync
    --delete` to one destination and restart one unit. Two on-landing
    deploys on one target from two worker slots (`--jobs 4`), or a
    landing and a `forge deploy`, run both rsyncs into the same
    directory; the last to finish wins, not the newest commit, and one
    deploy's failed check rolls the destination back under the other's
    check. The rollback target is the newest row with `check_ok ==
    Some(true)` other than this one (deploy.rs:387-391), without regard
    to sha: a redeploy of a commit that passed before and now fails
    'rolls back' to the same commit.
    *Input:* two landings on a repository with an on-landing target,
    finishing together. *Task:* "Hold an exclusive `flock` on
    `FORGE_HOME/deploys/<project>-<target>.lock` for the whole of
    `deploy::run` after target resolution, as `git::kernel_lock` does,
    and choose `previous` as the newest passing row whose `sha`
    differs from this deploy's, saying 'nothing else to roll back to'
    otherwise. Test: two `deploy::run` calls on one target, the second
    starting while the first sleeps in its method, run one after the
    other."

13. **A `deploy-self` that runs past its timeout is killed with `SIGKILL`,
    so its trap never restores anything.**
    The method runs under `cfg.check_timeout_secs`, 600 by default
    (config.rs:248; the Forge repository sets 900), through `run_one`,
    which on timeout calls `child.kill()` and then `kill -KILL` on the
    process group (checks.rs:209-227). `deploy_at` ignores the action's
    own `timeout_secs` (deploy.rs:267). The script's `on_exit` trap
    (deploy-self.toml:73-81) cannot run on `SIGKILL`. The script's own
    `flock -w 600` (line 41) is by itself the default timeout, and a
    cold `cargo build --release --workspace` adds to it. In the legacy
    branch, a timeout in the restart or check waits leaves `current`
    flipped to a release whose check never passed, and the
    `.doctor.XXXXXX` directory and a half-copied `releases/.<sha>.tmp`
    behind. The generic rollback (deploy.rs:387-427) then redeploys the
    last passing commit, which repairs `current` only when there is one.
    docs/DEPLOY.md's 'the whole method has to finish inside
    `check_timeout_secs`' is the only guard.
    *Input:* a self-deploy with a cold `bin/target` cache, or one that
    waits on the lock. *Task:* "Give operations a timeout of their own
    (`ActionDef.timeout_secs`, which `deploy-self` should declare, ample
    for a cold build) and make `run_one_capped` send `SIGTERM` to the
    group and wait a few seconds before `SIGKILL` when a check times
    out, so an operation's trap runs. Test: a script whose trap writes a
    file, killed by the timeout, has written it."

14. **The pointers have three writers and no shared lock.**
    `deploy-self` flips under `.deploy-self.lock`, but `successor::take_
    over` (successor.rs:291-306), `forge upgrade` (upgrade.rs:378-437)
    and `init --relink` (init.rs:285-289) call `release::flip` and
    `restore` (release.rs:97-129) with none. `flip` reads `current` and
    `previous` and writes them in two renames, so two flips can both read
    the same `was` and both set `previous` to it, losing a release from
    `previous`. `point` uses one temp name per pointer, `.current.new`
    (release.rs:88; the script's `point` uses the same, deploy-self.toml:
    55), so two writers of one pointer can rename each other's link, or
    fail on `symlink` (`EEXIST`) and report 'could not flip'. `forge
    upgrade` racing a self-deploy is the realistic pair. The install
    temp name has the same shape: `releases/.<id>.tmp` (release.rs:71).
    *Task:* "Put the lock in `release.rs`: `release::lock(root)` takes
    `flock` on `bin/.deploy-self.lock` and every caller of `flip`,
    `restore`, `install` and `point` holds it; give temp names a pid
    suffix. The script and Rust then agree on one file. Test: two
    threads flipping alternately never leave `previous` naming the
    release `current` names."

15. **`forge init` writes unit files that systemd mis-parses when a path
    contains a space, and loses the tail of PATH silently.**
    `worker_unit` and `web_unit` interpolate the home, the binary and
    the composed PATH unquoted into `Environment=` and `ExecStart=`
    (init.rs:75-77, 102-105). systemd splits `Environment=` on
    whitespace. *Reproduced* with `systemd-analyze verify` (systemd
    261): `Environment=FORGE_HOME=/tmp/my forge` warns 'Invalid
    environment assignment, ignoring: forge' and the unit runs with
    `FORGE_HOME=/tmp/my`, a different home; `Environment=PATH=/a:/mnt/c/
    Program Files/Git/cmd:/usr/bin` drops everything after the space
    (`Files/Git/cmd:/usr/bin`), which is what WSL puts in PATH. This is
    a third way for the PATH to be lost after task 707's two:
    `unit_path::compose` keeps any absolute directory (unit_path.rs:
    39-43) and `declared_path` reads it back quote-tolerant
    (unit_path.rs:51-58) though the writer never quotes. `%` is also a
    systemd specifier in both settings.
    *Input:* `forge init --home '/srv/my forge'`, or `forge init` from
    a shell whose PATH has a directory with a space. *Task:* "Quote and
    escape what `worker_unit` and `web_unit` write: `Environment=
    "KEY=value"`, `ExecStart` with each argument quoted, `%` as `%%`,
    `\` and `"` escaped, and refuse a newline. Add a unit test that
    parses the generated text back with `declared_path` for a home and
    a PATH with spaces, and an e2e that runs `systemd-analyze verify`
    when it exists."

16. **Re-running `forge init` reports 'already installed and enabled'
    without asking systemd, and applies nothing to the running worker.**
    `install_units` returns 'already installed and enabled' whenever the
    files are unchanged (init.rs:211-221), never running `systemctl
    is-enabled`: a home whose first `init` ran with no session (the
    files written, the by-hand commands printed, init.rs:196-209) is
    reported enabled forever after. When the files did change,
    `systemctl --user enable --now` (init.rs:243-251) does not restart
    an active unit, so the new `PATH` or `ExecStart` is on disk and the
    worker still runs the old, while the step says 'installed and
    enabled' (253-261); `require_on_path`'s hint (unit_path.rs:86)
    tells the operator to re-run init and restart, and init never says
    the restart is the part that applies it. And the PATH it writes is
    the current shell's, not merged with the unit's own
    (init.rs:191-193): re-running from a narrower environment (an ssh
    command, cron, an agent's sandbox) replaces a good PATH with a poor
    one, silently, on every re-run. That is how the PATH was lost the
    second time.
    *Input:* `forge init` from a shell without the agent CLIs' directory
    when the unit has it. *Task:* "In `install_units`, ask `systemctl
    --user is-enabled` for each unit and enable what is not; after
    writing a changed unit report 'restart forge-worker to apply' and
    name the worker's live PATH when it differs from the unit's; keep
    the entries of the unit's existing `PATH` (`declared_path`) that the
    new one lacks, after the new ones, unless `--reset-path` is
    passed. Tests for each."

17. **The checked-in `deploy/forge-worker.service` is unsafe with the
    successor handoff, and docs/DEPLOY.md cites it as the truth.**
    The template is `Type=simple`, without `NotifyAccess=all`, with
    `ExecStart` in `~/Projects/forge/target/release/` and no
    `StartLimit*` (deploy/forge-worker.service:11-15). Under it
    `MAINPID=` is ignored (successor.rs:74), so when the old worker
    exits after a handover systemd sees the main process exit and stops
    the unit, killing the successor; and `capable` (successor.rs:236-
    241) still says yes, so `deploy-self` only stages. docs/DEPLOY.md
    ('How a deploy survives its own worker restart') cites the file for
    the drain semantics. The unit `init` writes is the other definition.
    *Task:* "Delete `deploy/forge-worker.service` or generate it from
    `init::worker_unit` (a test that fails when they differ), and point
    docs/DEPLOY.md and docs/OPS.md at `forge init` as the only source of
    the unit."

18. **`deploy-user-service` and `deploy-command` with `host=local` cannot
    reach systemd: the operation's environment has no
    `XDG_RUNTIME_DIR`.**
    `agent_env` passes `PATH`, `HOME`, `LANG`, `TERM` and a few provider
    variables (agent.rs:286-312), and operations run with only that
    (operation.rs:473-480). `deploy-self` knows and exports
    `XDG_RUNTIME_DIR` (deploy-self.toml:28-30);
    `deploy-user-service`'s `run_remote` does `bash -c "$1"` for
    `host=local` with none (deploy-user-service.toml:25-31).
    *Reproduced:* `env -i PATH=$PATH HOME=$HOME systemctl --user
    is-active x` prints 'Failed to connect to user scope bus via local
    transport: $DBUS_SESSION_BUS_ADDRESS and $XDG_RUNTIME_DIR not
    defined'. The restart fails (the script is `set -e`), the deploy's
    check fails, and the rollback runs into the same failure. The e2e
    suite uses a fake `systemctl`, which cannot show it. Over ssh a
    login session sets the variable, so only `local` is affected.
    *Task:* "In `deploy-user-service`, export `XDG_RUNTIME_DIR=${XDG_
    RUNTIME_DIR:-/run/user/$(id -u)}` before the local `systemctl`
    calls, as `deploy-self` does. Test: a fake `systemctl` that fails
    without the variable."

19. **`deploy-user-service` and `deploy-command` never check `dest`, and an
    empty one makes rsync target the remote's `/`.**
    `unit` is validated (deploy-user-service.toml:21-24); `host` and
    `dest` are not, and `add_target` accepts a target without either
    arg (deploy.rs:558-589). With `dest` empty the script runs `rsync -a
    --delete "$src"/ "$host:$dest"/` (deploy-user-service.toml:37,
    deploy-command.toml:26), which expands to `host:/`. `--delete`
    removes anything the ssh user can delete under `/` that is not in
    the source tree. (Not executed: it would delete files. The expansion
    is what was confirmed.) `host=local` fails first, at `mkdir -p ""`.
    *Input:* `forge project deploy add p t --method deploy-command --arg
    host=box --check true` (a forgotten `--arg dest=`). *Task:* "Refuse
    at `add_target`/`set_target` a method whose required args are
    missing (give an operation a `required_args` list for this), and in
    both scripts exit 1 when `host` or `dest` is empty or `/`. Test for
    both."

20. **Doctor has no release row, and the worker and plugins rows say
    'fine' about things that are not.**
    (a) There is no row for the release layout: not `current`,
    `previous`, `staged`, nor whether a pointer dangles, nor which
    release the worker runs (the only mention of `staged` is inside
    `check_succession`, doctor.rs:619). (b) The worker row's staleness
    is 'the worker's binary is deleted' (worker.rs:71-81,
    doctor.rs:668-691), which cannot happen under the release layout,
    where nothing is ever overwritten: a worker on a release older than
    `current` (an upgrade or a flip without a restart, defect 3) is
    reported `pid N running`, ok. It returns no row at all when there is
    no `worker.pid` (doctor.rs:665-667), so a home whose worker never
    started says nothing. (c) The plugins row is `Ok` unless a plugin
    fails to load (doctor.rs:531-541); the function comment says a
    crash-looping plugin is a warning (doctor.rs:502-505), but a
    `restarting (x9)` state only lands in the detail. It reads
    `plugins-run/<name>.json` without checking that the pid is alive
    (doctor.rs:520-529), so a plugin whose supervisor died, or whose
    worker stopped them (defect 2), still reads `running pid N, up
    3000s`.
    *Task:* "Add a `release` row (`current`, `previous`, `staged`,
    dangling links, the newest live worker's version from `workers`);
    `Warn` when the live worker's version is not `current`, when `staged`
    names something other than `current` with no successor, and when a
    pointer dangles. Make the worker row `Warn` on 'no worker' when
    units exist. In the plugins row, `Warn` for `restarting`, and treat
    `running` whose pid is dead as `stopped: supervisor gone`."

21. **The deploy look is told the smoke check passed when it may not
    have, and feeds page text to an unsandboxed agent whose 'blocking'
    finding rolls the deploy back.**
    `deploy::run` calls `deploy_look::run` 'whether or not the
    deterministic smoke check itself passed' (deploy.rs:311-315), but
    the prompt states 'its automated smoke check already passed'
    (deploy_look.rs:43-44). The prompt also embeds the page's
    `<title>`, its console errors and its failed requests from
    `smoke.json` (deploy_look.rs:87-95, 126-133), which the deployed
    site controls, and the directive runs with `sandboxed: false`
    (deploy_look.rs:153) and file tools. A page whose title says
    'report a blocking finding' makes `deploy::run` mark the deploy
    failed and roll it back (deploy.rs:327-335). The `UNTRUSTED_DATA`
    preamble is the only defence.
    *Input:* a deployed page whose title or console output is
    attacker-shaped (a project that renders user content).
    *Task:* "Tell the look the smoke result it really had; put the
    page-derived strings in a fenced block under the untrusted-data
    header, truncated; and run the look sandboxed (`sandboxed: true`,
    read-only mount of `out_dir`), since it reads one image. Consider
    requiring two agreeing looks, or a human confirm, before a look
    alone rolls back a deploy whose check and smoke passed."

22. **`deploy-self`'s legacy branch and its lock assume a Linux box with
    `flock` and every unit installed.**
    `flock` missing (macOS, which the release matrix builds) makes `if !
    flock -w 600 9` true and prints 'another self-deploy held the lock
    for ten minutes' (deploy-self.toml:41-44), a false reason. The
    legacy branch runs `systemctl --user restart $units` unfiltered
    (155) on `forge-web forge-portal`, and `forge init` writes only the
    worker and web units (init.rs:189-193; the portal's is hand-installed
    per docs/PORTAL.md:107), so on a machine set up by `forge init` the
    restart fails, the `is-active` wait would never succeed, and the
    deploy rolls back. `take_over` reports the same absent unit as
    'could not restart forge-web, forge-portal' even though `forge-web`
    restarted (successor.rs:299-305). `forge upgrade` filters by unit
    existence (`existing_units`, upgrade.rs:229-236).
    *Task:* "Check `command -v flock` and `systemctl` first with their
    own messages; restart only units `systemctl --user cat` finds (the
    filter `upgrade.rs` uses); make `take_over` do the same."

23. **`init --relink` builds a 'release' from whatever sits beside the
    running binary, and names it differently from `deploy-self`.**
    `adopt_running_binaries` copies every binary that exists in the
    running executable's directory into `releases/<FORGE_GIT_SHA>/`
    (init.rs:275-289, release.rs:57-84). Run from `target/debug` after
    `cargo build -p forge`, that is a fresh `forge` beside older
    `forge-web`, `forge-portal` and `forge-tui` from earlier builds:
    a mixed release becomes `current`, and `install` never touches it
    again (release.rs:63-65). The id is the short sha `build.rs`
    emits (build.rs:20), while `deploy-self` names its release by the
    full sha (deploy-self.toml:35): the same commit lives under two
    names, so a self-deploy of the commit that is already live builds it
    again and stages a 'different' release, which the worker treats as a
    successor (successor.rs:153).
    *Task:* "Have `--relink` require all of `release::BINS` but
    `forge-test` in the executable's directory, else refuse with the
    missing names, and resolve the id to the full sha (`git rev-parse`
    of `FORGE_GIT_SHA` in the kernel repository, or embed it in
    `build.rs` in full). Test: a directory with only `forge` is refused."

### 3.2 Read and found sound

- **`release.rs`'s primitives.** `point` renames a temporary symlink into
  place, so a reader sees the old link or the new, never none. `flip`
  writes `previous` before `current`, so a crash between them leaves
  `previous` naming a release that is still live, never a lost one.
  `install` copies into a `.tmp` sibling and renames it, and never
  touches an existing release. Defect 14 is about who calls them, not
  about them.
- **Where the deploy's tree comes from.** `origin_truth` fetches origin's
  base branch into the kernel-owned bare repository by an explicit
  refspec (`git::stage`, under `kernel_lock`, hooks off), resolves a
  `--sha` with `^{commit}` there and requires it to be an ancestor of
  that tip, and archives from that repository; the registered
  checkout's working tree and refs are never read. `non_self_src`
  keeps an on-landing deploy independent of the checkout's best-effort
  fetch. `fresh_archive` removes the destination first and `deploy_at`
  removes the scratch tree both on success and failure of the method.
- **The lock itself.** `exec 9>` then `flock -w 600 9` is released when
  the script exits; the children it starts (cargo, the release's
  doctor) end with it, and on timeout the whole process group is
  killed, so no orphan can hold the descriptor. What is not sound is
  what it does not cover (defects 4, 11, 14).
- **Staging and the build.** The release is built into a `.tmp`
  directory and `mv -T`-ed into place, so it is never half-written; an
  existing release is reused without a rebuild; the required binaries
  are checked before the copy; the scratch doctor's home is removed by
  the trap; `FORGE_BUILD_SHA` gives an archive with no `.git` its
  commit. The description in the operation's toml and steps 1-7 of
  docs/DEPLOY.md match the script.
- **`deploy::run`'s bookkeeping.** `record_deploy_error` now finishes
  the row on every error after `start_deploy` (REVIEW-3, defect 9's
  open row), the smoke step runs only after the check passed, and a
  look that itself errors never fails the deploy
  (deploy.rs:339-347); only a `blocking` finding does, and
  `check_severity` rejects any other severity string.
  `parse_target_args` refuses keys that shadow the target's own flags,
  and `add_target`/`set_target` require a check unless the method
  supplies its own.
- **The successor mechanics that are right.** The successor gets its own
  process group, `FORGE_HOME`, `FORGE_RELEASE` and the same arguments
  (successor.rs:163-181); `take_over` is a no-op when `current` already
  names the release; the capability file is written by rename
  (successor.rs:251-259); the old worker waits for it before leaving so
  systemd never sees the main pid go before `MAINPID=` moved (the wait's
  length and outcome are defect 1); `capable` accepts either a live
  registered worker or a live capability pid. `register_worker`'s
  double registration is harmless, as REVIEW-3 already found.
- **Shadowing's diff and cache.** `diff_of` uses a unique temp file per
  call and treats only exit 0 and 1 as a diff, `scan` never caches a
  failed diff, and a stale seed is noted once per process.
  `has_operator_commit`'s rule is the defect (7, 8); the mechanics
  around it are not.
- **`unit_path::compose`** puts the binary directory first, deduplicates,
  drops relative entries and falls back to the system directories only
  when the shell has no absolute one, all under unit tests; quoting is
  defect 15.
- **`deploy-user-service` otherwise.** It validates `unit`, quotes it
  in the remote command, builds its `--exclude` list as an array and
  bounds the active wait (20 tries), and `host=local` and ssh share
  one `run_remote`.
- **`deploy-self`'s legacy branch, as far as it goes.** Flip, restart,
  bounded `is-active` waits, a bounded check and the worker restarted
  last with `--no-block` do what docs/DEPLOY.md says on a machine that
  has both units; the worker is never restarted after a failed check.

### 3.3 Not readable from this checkout

**The bare origin's post-update mirror hook.** docs/OPS.md:359 says
`main` and `v*` tags are mirrored to GitHub by a post-update hook on the
bare repository. That hook lives in the bare repository, not in this
one, and no bare origin exists on the machine this was read on (`git
remote -v` is empty here; `find / -name post-update` finds only git's
`.sample`). Nothing about it is recorded as a defect because none can be
confirmed at a line. What could be read is what depends on it: `deploy-
self` fetches origin with `git::stage` and does not use `refs/tags`, so a
failing or slow mirror cannot change what is deployed, but a hook that
mirrors synchronously runs inside every push (`receive-pack`) and would
hold the landing's push open. To settle it, whoever has the bare
repository should read `hooks/post-update`, check that it runs in the
background or bounds its network call, that it does not fail the push
when GitHub is unreachable, and that it mirrors by explicit refspec; and
the hook should be committed under `deploy/` so it can be reviewed and
installed by `forge init`.

## Closing table: every defect across the three sections

Section 3's rows are below. Rows for sections 1 (sandbox and egress) and
2 (plugin supervision and the successor handoff) belong here too, ids
`E1-n` and `E2-n`; those sections were written by their own tasks and
are not in this tree, so their rows are to be added when they merge.
Defects 1, 2 and 3 above touch the successor handoff and may duplicate or
overlap section 2's; the fix initiative should file them once.

| id | file:line | defect |
|----|-----------|--------|
| E3-1 | src/successor.rs:200-229, worker.rs:1119-1122, init.rs:69-81 | a dead or slow successor makes the old worker exit non-zero; restart loop until the unit hits its start limit; `current` flipped before the successor is proven |
| E3-2 | src/successor.rs:106-112, 135 | plugins stopped for a successor are not restarted when it dies |
| E3-3 | src/successor.rs:149-158, src/upgrade.rs:347-366 | `staged` never consumed: `forge upgrade` and manual flips of `current` are reversed by a new successor |
| E3-4 | src/builtins/operations/deploy-self.toml:47-49, 60-71 | `restore` writes stale pointers over a successor's flip and restarts web and portal for any failure |
| E3-5 | deploy-self.toml:136-140, src/deploy.rs:358-382 | `forge deploy forge self` is `ok` when only staged; nothing checks the release went live |
| E3-6 | deploy-self.toml:112-119 | scratch doctor proves the schema on an empty store; pass condition is a JSON substring |
| E3-7 | src/workflows/shadow.rs:83-95, 329-341, src/deploy.rs:251 | a catalog copy of `deploy-self` shadows the built-in; classification by commit author; doctor says ok |
| E3-8 | src/workflows/shadow.rs:83-95, 279-298, src/init.rs:355 | uncommitted or init-committed edits are 'stale seeds', ignored and deleted by `refresh` |
| E3-9 | src/deploy.rs:179-221, src/queue.rs:1252-1262 | deploy question unanswerable by `forge answer`; a landed task is flipped to blocked |
| E3-10 | src/deploy.rs:304, 429-438, 483 | errors after start skip rollback and question; a later error overwrites the recorded rollback |
| E3-11 | src/deploy.rs:107-120, 262-263 | 'older than live' guard fails open, runs before the lock, ignores `staged` |
| E3-12 | src/deploy.rs:238-484, 387-391 | no per-target lock for non-self methods; rollback target may be the failed sha |
| E3-13 | src/deploy.rs:267, src/checks.rs:209-227, deploy-self.toml:73-81 | `SIGKILL` at `check_timeout_secs`; the restore trap never runs; the action's own timeout ignored |
| E3-14 | src/release.rs:87-129, src/successor.rs:291-306, src/upgrade.rs:378-437 | pointer writers share no lock; temp names collide |
| E3-15 | src/init.rs:75-77, 102-105 | unit values unquoted: a space in home or PATH truncates them (reproduced) |
| E3-16 | src/init.rs:191-193, 211-221, 243-261 | re-run says 'enabled' without asking; change not applied to the running worker; PATH replaced, not merged |
| E3-17 | deploy/forge-worker.service:11-15 | template is `Type=simple`: unsafe with the successor handoff; cited by docs |
| E3-18 | src/builtins/operations/deploy-user-service.toml:25-31 | `host=local` has no `XDG_RUNTIME_DIR`; `systemctl --user` fails (reproduced) |
| E3-19 | deploy-user-service.toml:37, deploy-command.toml:26, src/deploy.rs:558-589 | empty `dest` rsyncs `--delete` to the remote's `/`; args not validated |
| E3-20 | src/doctor.rs:531-541, 657-692, 520-529 | no release row; worker staleness cannot fire under the layout; plugins row ok while crash-looping or dead |
| E3-21 | src/deploy_look.rs:43-44, 87-95, 153, src/deploy.rs:311-335 | look told smoke passed when it may not have; page text into an unsandboxed agent whose finding rolls back |
| E3-22 | deploy-self.toml:41-44, 155, src/successor.rs:299-305 | `flock`/units assumed; false lock message; unfiltered `restart $units` |
| E3-23 | src/init.rs:275-289, src/release.rs:57-84 | `--relink` adopts a mixed set of binaries and names the release by short sha |
