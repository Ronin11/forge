# Backlog: Forge 2 as its own bench

Small, precisely specified chores, each one task. Every task runs `reviewed`
with no landing (there is no remote here), so a human merges what verified.
The point is the measurement as much as the work: workflows compared,
`cheap` tried on the precise ones, the journal on and off in alternation.

Precise and small (candidates for `cheap`):

1. **`forge journal --json`.** The journal as structured entries (task, attempt,
   step, state, said, found) beside the text form. Test in tests/e2e.rs.
2. **Events keep two rolled generations**, not one: `events.jsonl.1` becomes
   `.2` before the roll. Test in the events e2e.
3. **`tools::family` handles `bash -c` and `sh -c` wrappers** by classifying the
   inner command. Unit tests.
4. **`forge show` prints the lineage even for a root task with children.**
   Today only a task inside a chain longer than one shows it. Test.
5. **`forge stats --tools --step <name>`** limits the report to one step. Test.

Medium (`reviewed`):

6. **Token usage per attempt.** Record input, output, cache-read, and
   cache-write tokens from the CLI's result frame on the attempt; show them in
   `trace --json` and a `TOKENS` column in `stats`. Test with a fake that emits
   a `usage` object.
7. **Resume a failed attempt's session, as an option.** `--resume-on-failure`
   on add/run: the next attempt after a checks failure continues the same CLI
   session with the feedback, the way a capped attempt does today. Recorded in
   the attempt's inputs; preserved by retry. e2e with a fake that checks for
   `--resume`.
8. **Operation output in full, on request.** `output = "full"` in an operation
   file keeps the whole stdout/stderr instead of a 40-line tail, capped at
   1 MB. Test via the operations e2e.
9. **`forge doctor` reports the event log size and the oldest attempt log**,
   with a hint when logs pass 1 GB. Test.
10. **Cost anti-pattern rows in the audit.** From an attempt's tool facts:
    "capped with a dirty tree", "read the same file N times", "explored more
    than 15 calls before the first edit", each with the attempt's cost. Shown
    by `forge show` under the existing diagnosis. Unit tests in audit.rs.
