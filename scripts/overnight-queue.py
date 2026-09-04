#!/usr/bin/env python3
"""Overnight system-test queue: real work that also stress-tests the factory.

Two tracks, all integrate=false (branches only, nothing merges), autonomy=auto:
  A. Forge self-fixes — precise implement tasks for the findings this session
     surfaced. Wake up to reviewable fix branches.
  B. equitizr flow experiment — real, verifiable objectives run through the role
     flows (each ends in r-eval), so the morning has both product branches AND
     comparative data on which flow composition scores best. One objective is run
     through two flows as a controlled comparison.

Budget hard-stops pace it; over-provisioning is intentional so the factory never
idles at 3am. Re-run safe (dedupe guards identical prompts within 24h; pass
--force upstream if needed).

  python3 scripts/overnight-queue.py
"""
import json
import socket

SOCK = "/home/ronin/.forge/forge.sock"


def call(method, path, body=None):
    payload = json.dumps(body).encode() if body is not None else b""
    req = (
        f"{method} {path} HTTP/1.1\r\nHost: localhost\r\n"
        f"Content-Type: application/json\r\nContent-Length: {len(payload)}\r\n"
        f"Connection: close\r\n\r\n"
    ).encode() + payload
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(SOCK)
    s.sendall(req)
    data = b""
    while True:
        chunk = s.recv(65536)
        if not chunk:
            break
        data += chunk
    s.close()
    head, _, body_bytes = data.partition(b"\r\n\r\n")
    return int(head.split(b" ")[1]), body_bytes.decode(errors="replace")


# --- Track A: Forge self-fixes (implement mode, branches only) ---
FORGE_FIXES = [
    ("opus", 50, "L0 greenfield exemption",
     "In internal/worker/verify.go, the L0.2 changes-consistency check (the block that "
     "sets `l0:changes_mismatch`, ~lines 217-242) requires exact bidirectional set "
     "equality between the declared changes[] envelope and git's ChangedPaths. This is "
     "wrong for greenfield / model.WritesNewProject mode, which creates hundreds of new "
     "files and legitimately reports coarse summary changes — EVERY greenfield build "
     "false-fails as unverified=l0:changes_mismatch despite passing all its declared "
     "checks. Fix: when the write scope is model.WritesNewProject, skip (or relax to a "
     "subset) the L0.2 changes-exactness comparison — greenfield is L2-verified via its "
     "declared checks anyway. Keep the check intact for every other scope. Add a test "
     "covering a WritesNewProject envelope with coarse changes that used to fail and now "
     "passes, and assert a normal-mode mismatch still fails. Follow docs/VERIFICATION.md. "
     "Make `just check` pass."),
    ("opus", 60, "reap verify-phase orphans",
     "Forge leaks child processes on attempt-end and worker-stop: headless chromium (from "
     "L2/verify browser checks, user-data-dir under /tmp) and preview HTTP servers "
     "(python -m http.server / npm run preview) spawned during a build survive after the "
     "attempt finishes and after the worker unit stops (journald shows 'Unit process N "
     "(chromium) remains running after unit stopped'). Same class as the plugin FD-leak "
     "fixed via Supervisor.Shutdown. Ensure every process an attempt spawns is in its own "
     "process group and is killed (SIGTERM then SIGKILL) when the attempt ends AND when "
     "the worker shuts down, so nothing outlives its attempt. Add a test where feasible. "
     "Make `just check` pass."),
    ("sonnet", 40, "worker skip-one-bad-repo",
     "The worker refuses to START if ANY registered repository fails validation (e.g. "
     "'repository X: read origin remote: No such remote origin') — one misconfigured repo "
     "in worker.toml bricks the whole worker and all its work. In internal/worker/"
     "runner.go where it validates repositories and reads their origin at startup, change "
     "it to SKIP-and-WARN a bad repo (log a clear error, exclude it) instead of failing "
     "the entire worker, as long as at least one repo is valid. Add a test with a mix of "
     "valid and invalid repos asserting the worker starts with the valid ones. Make "
     "`just check` pass."),
    ("sonnet", 40, "greenfield double-nesting",
     "Greenfield builds land double-nested at <projects_root>/<slug>/<slug>/ with a stray "
     "forge.toml at the outer level, because the agent builds in a subdirectory named "
     "after the project instead of at the worktree root and greenfieldMove moves the whole "
     "worktree. Fix so a finished greenfield project lands cleanly at "
     "<projects_root>/<slug>/. Prefer a move-side fix in internal/worker/greenfield.go "
     "greenfieldMove (flatten a single top-level subdir whose name matches the project) so "
     "it is robust to agent behavior. Add a test for the flatten logic. Make `just check` "
     "pass."),
]

# --- Track B: equitizr flow experiment (workflow runs, branches only) ---
# (flow_name, objective). flow-quick/standard/deep already exist (setup-role-flows.py).
EQUITIZR_FLOWS = [
    ("flow-standard",
     "Add a thorough unit test suite for lib/matching.ts: exact match, token-containment, "
     "Jaro-Winkler fuzzy matching, alias handling, and the stale-brand-tag downgrade (an "
     "OSM brand tag that contradicts the visible name). Do NOT change matching behavior — "
     "only cover it. Use the project's test runner."),
    ("flow-quick",
     "Add unit tests for the confidence scoring (name-match quality x ownership sourcing "
     "verified/reported/scraped x stake type majority/franchisor/minority, and the "
     "drop-below-55 rule). Cover the boundary (54 vs 55). Do not change behavior."),
    ("flow-standard",  # controlled dup of the confidence objective through a heavier flow
     "Add unit tests for the confidence scoring (name-match quality x ownership sourcing "
     "verified/reported/scraped x stake type majority/franchisor/minority, and the "
     "drop-below-55 rule). Cover the boundary (54 vs 55). Do not change behavior."),
    ("flow-deep",
     "Harden the SEC ingest pipeline (scripts/ingest-sec.ts): add retry-with-backoff on "
     "EDGAR/rate-limit failures, skip-and-log firms that cannot be resolved instead of "
     "aborting the whole run, and make the run resumable/idempotent. Keep the data model "
     "unchanged."),
    ("flow-deep",
     "Implement a FIRST version of community-submitted ownership claims per "
     ".forge/notes/community-data-and-disputes.md: accept a claim with a REQUIRED source "
     "URL, store it as a lowest-weight 'community' edge tagged unverified (weight ~0 "
     "without a source), and surface a 'community claim — unverified' marker in the trace/"
     "entity view. No promotion gate or moderation UI yet — just the data model, intake, "
     "and the honest label."),
]


def main():
    print("== Track A: Forge self-fixes (implement, integrate=false) ==")
    for model, turns, title, prompt in FORGE_FIXES:
        body = {
            "prompt": prompt, "repositories": ["forge"], "mode": "implement",
            "model": model, "autonomy": "auto", "class": "normal", "priority": 50,
            "max_turns": turns, "timeout_seconds": 3600, "integrate": False,
            "title": f"overnight-fix: {title}",
        }
        st, resp = call("POST", "/api/v1/tasks", body)
        print(f"  [{model:6}] {title:26} -> {st}")
        if st >= 400:
            print("     ERROR:", resp[:200])

    print("== Track B: equitizr flow experiment (integrate=false, r-eval scored) ==")
    for flow, objective in EQUITIZR_FLOWS:
        body = {"repositories": ["equitizr"], "objective": objective}
        st, resp = call("POST", f"/api/v1/workflows/{flow}/run", body)
        tag = objective.split(":")[0][:40] if ":" in objective else objective[:40]
        print(f"  {flow:15} <- {tag}… -> {st}")
        if st >= 400:
            print("     ERROR:", resp[:200])


if __name__ == "__main__":
    main()
