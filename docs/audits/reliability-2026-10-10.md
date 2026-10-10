# Reliability audit, pass one: silent failure, 2026-09-11 to 2026-10-10

Read-only. Every number below comes from records Forge already keeps: the
store (`forge.db`), the event log (`events.jsonl`) and the worker's journal
(which only reaches back to 2026-10-03; journald retention). The measurement
script is beside this file as `reliability-2026-10-10-measure.py`; it
prints ok/runs per subsystem per day and flags days of three or more runs
with no success.

The question asked was not "what is broken" but "what broke completely and
how long did it take anyone to notice". The answer, in every case, is: days,
and the evidence was on disk the whole time.

## Findings

| # | Subsystem | Failed completely | Noticed | Evidence that existed, unread |
|---|---|---|---|---|
| 1 | Supervisor | 2026-09-29 → today (12 days) | 2026-10-10 | `attempts` rows with step=supervisor, state=agent_failed: 10/10 ok on 09-27, 14/14 on 09-28, 2/25 on 09-29, 0 of every run since 10-01 |
| 2 | Worker (restart loop) | 2026-10-06 → 10-09 | 2026-10-10 | journal: 2,075 / 7,363 / 6,680 / 5,862 systemd restarts per day; `NRestarts=21985` |
| 3 | Worker (exit at task end) | 2026-10-04 → 10-10 | 2026-10-10 | journal: bare "Permission denied (os error 13)" then drain, ~30 times |
| 4 | gc for landed tasks | since the first landing | 2026-10-10 | `gc-nightly` job ok 15/15 while `forge gc --dry-run` kept 474 of 478; 96 GB of landed worktrees; `published()` can never be true without a `push_remote` |
| 5 | Signal plugin, inbound | since enabled 2026-09-27 (never worked) | 2026-10-10, by accident | `messages`: 329 outbound, 0 inbound ever; signal-cli warns "last received 22 days ago" (09-18, before the plugin existed); the receive loop sends stderr to /dev/null, and so do sends |
| 6 | Signal plugin, process | 6 crash loops, older instance | never | plugin log: `FORGE_BIN` pointed at `~/Projects/forge/target/release/forge (deleted)`, the build-tree binary cargo replaces; current instance uses `bin/current/forge` |
| 7 | doctor-daily, drift-weekly jobs | 09-28, 09-30, 10-01, 10-05 | request 1432 open since 10-01 | job verdict: check `clean` failed with "[forge] no output captured; the check exited code 1 with nothing on stdout or stderr". A failure that says nothing. |
| 8 | Provision e2e test on this host | 2026-10-09 → 10-10 | 2026-10-10 | `check` events: forge `test` failing on every task; mise shim refusing a foreign HOME; cause was environmental, looked like agents failing |

Smaller things the table turned up:

- The `test` check is not deterministic on this host under load. Landing this two-file documentation branch through `forge adopt` failed three times on three different tests: `naming::forge2_does_not_creep_back…` (real, the script named the data directory), then `agent::relaunch::…bwrap_loses_the_bind_mount_race` and `checks::…not_relaunched` (both pass on main in 0.02 s from a shell), then `resume::the_worker_holds_while_a_rate_window_is_at_its_cap…` after 600 s. With ten tasks running at once, the e2e suite's timing assumptions fail. This is the mechanism behind Forge's 30% land rate: most of its failures are the `test` check, and a share of those are the suite, not the change. Pass two's detector 4 catches the environmental case across tasks; the suite itself needs its timing-sensitive tests isolated or given load-aware timeouts.

- `step/integrate` reads 0/45 ok. Those rows are written only when a merge's checks fail, so the ratio is by construction; the op-level number is 1054/1245. The row is misleading in any stats view and should be labelled.
- `changelog-line` 0/8 (all on 09-17) and `engineering-weekly` skipped 3 of 4: probably retired or by design; confirm, then remove or fix.
- Egress refusals: 1,634 for `api.anthropic.com` (all finding 1 and the login refresh probe), 4,124 for Datadog intake, ~1,500 for OpenAI blob hosts and chatgpt.com, 152 for google. The first kind is an error; the rest is telemetry noise. Nothing distinguishes them today.
- Forge's own tasks land 30% of the time over the last two weeks (3/25 today, 8/21 on 10-06, 16/44 on 10-04); Exploration lands 75%. Visible in `forge log`, so not silent, but there is no trend anywhere.
- The journal is the only record of findings 2 and 3, and it keeps seven days. The supervisor's break date came from the store. Detectors have to read the store and the event log, not journald.

## Key series, last two weeks (ok/runs per day)

```
                                          09-27 09-28 09-29 09-30 10-01 10-02 10-03 10-04 10-05 10-06 10-07 10-08 10-09 10-10
supervisor / run                          10/10 14/14  2/25     -   0/3   0/1   0/2   0/5   0/3   0/6     -     -     -   0/4
worker / restarts (journal)                   -     -     -     -     -     -     1     1     3  2075  7363  6680  5862     2
egress-refused / api.anthropic.com            -     -     -     -     -     -   461   164   805    34     -     -    34   136
gc / worktree-removed                     36/36 57/57 105/105 73/73 28/28 41/41  1/1 46/46 55/55 81/81 25/25   5/5 14/14 36/36
job / gc-nightly                            1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1
job / doctor-daily                          1/1   1/1   1/1   0/1   0/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1   1/1
check / forge:test                       108/137 225/330 162/242 2/2   5/7     - 20/24 34/78  6/10 23/50     -   2/3     - 15/50
check / exploration:test                      - 18/18 24/24   2/2 103/103 33/33  3/3 50/50 18/18 135/137   - 31/31 83/83 116/116
step / code                               65/162 171/339 110/226 1/1   3/7   1/1 33/43 72/123 18/23 157/214  -   1/4     - 18/58
step / review                             44/69 125/166 75/113  1/1   2/4   1/1 27/39 46/52   7/8 82/94     -   1/1     -   6/8
step / adopt                                  -     - 24/27 26/27 51/51 14/14     -     -     -   2/2   4/4 24/25 40/48 46/50
task / forge                              38/95 75/181 52/328  1/2   2/4     -  7/15 16/44   2/4  8/21     -   1/1     -  3/25
task / exploration                            -  8/10 12/19   1/1 49/60 16/17   1/3 25/25   7/7 55/72     - 14/15 35/40 50/98
deploy / forge/self                       42/42 74/77 53/53   1/1   2/2     -   7/7 16/16   1/2   5/8     -   0/1     -   3/3
```

Healthy over the whole window, for the record: clone 1677/1677, push 1318/1318, repo-map 1332/1332, setup 1404/1424, land 1041/1054, deploy equitizr/prod 34/34, backup-daily 20/24, provider probes and holds (login expiry) all followed up.

## What pass two and three should build

Pass two, one task: a detector per row of the findings table, each a `forge doctor` row and an event, each computed from the store or the event log, never from journald:

1. Supervisor: attempts in the last 24 h with step=supervisor and none succeeded, while at least one task blocked → WARN naming the last failure's reason.
2. Worker: restarts since the last doctor run (systemd `NRestarts` delta, recorded in the store each tick so it survives journal rotation) above a small number → WARN with the last exit reason.
3. Any step kind (code, review, verify, supervisor, land) at zero successes over its last N runs when N ≥ 5 → WARN.
4. A named check failing on every task of a project within 24 h, across different branches → WARN "environmental, not the agents" with the check's tail.
5. Worktrees: total bytes under `worktrees/` and the count of retained trees whose task landed, recorded daily; WARN on a threshold or on three days of growth. `gc-nightly` reports those numbers in its verdict instead of only "ok".
6. Plugins: each plugin records the time of its last successful inbound poll and last successful outbound send in its state dir; doctor WARNs when the inbound age exceeds 10× the poll interval. Subprocess stderr goes to the plugin log, never `/dev/null`.
7. Egress: a refusal of a configured provider's own model host is an ERROR event; refusals of anything else are counted, not logged line by line.
8. Jobs: a job in `needs_human` or `failed` older than 48 h → WARN; a check that produced no output at all ("no output captured") is its own named failure, not folded into the check's name.
9. Stats: `step/integrate` labelled as failure-only; a per-project land-rate trend in `forge stats`.

Pass three, one task: doctor's own failure and its WARN rows reach the operator without being asked. doctor-daily's job failing (finding 7) is the test case: a doctor that fails must produce a notification through the notify and signal plugins and a line on the portal's front page, and every WARN row above does the same once, with a daily digest rather than a repeat. The signal plugin's inbound side must be made to work and prove it (finding 5), since it is the channel the notifications ride on.
