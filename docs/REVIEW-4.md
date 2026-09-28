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
