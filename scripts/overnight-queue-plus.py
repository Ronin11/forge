#!/usr/bin/env python3
"""Overnight queue, round 2: the bigger foundational asks — integration/e2e tests
with a merge/release gate, a TUI, and a modularization PLAN (not a blind refactor).

All integrate=false (branches only), autonomy=auto. Forge-repo tasks serialize on
the forge path-lease, so these run as a paced pipeline through the night. The new
asks lead (priority 55/52) so they run before round 1's self-fixes (priority 50).

  python3 scripts/overnight-queue-plus.py
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


# (mode, model, priority, max_turns, title, prompt)
TASKS = [
    ("implement", "opus", 55, 70, "integration tests",
     "Add an INTEGRATION test suite for Forge exercising the daemon<->worker<->store flow "
     "end to end with the fake-claude executor. FIRST survey existing integration/smoke "
     "coverage (docs/SMOKE.md and any *_test.go that already spin up the daemon) so you "
     "EXTEND rather than duplicate. Then add hermetic tests (temp FORGE_HOME, no network, "
     "fake executor) that start the daemon + register a worker (in-process or via the "
     "existing harness), submit a task over the HTTP-on-unix-socket API, and assert the "
     "full lifecycle to a terminal state — cover (a) a clean success that verifies, (b) a "
     "verification failure (unverified), and (c) a human-question / waiting_human path "
     "answered programmatically. Expose them via a `just test-integration` recipe in the "
     "Justfile. Keep the existing `just check` green. integrate is off — commit on your "
     "branch."),
    ("implement", "opus", 55, 70, "e2e tests + release gate",
     "Add an END-TO-END test suite that drives the REAL `forge` CLI against a REAL daemon "
     "on a temp FORGE_HOME (fake-claude executor, no network): cover the operator journey "
     "— daemon start, repo add, routine add, task add, task show, queue, usage, task "
     "cancel — asserting exit codes and key output. Then wire a merge/release GATE: a "
     "`just check-release` recipe (and, if a CI config exists under .github or similar, a "
     "workflow) that runs the existing `just check` plus the integration and e2e suites, "
     "meant to run on merge to main and on version tag. Document when it runs (docs/"
     "SMOKE.md or a new docs section). A sibling task adds `just test-integration`; if that "
     "recipe is absent, define the e2e recipe so the two compose without clobbering each "
     "other. Keep `just check` green. integrate off; branch only."),
    ("implement", "opus", 52, 100, "forge TUI",
     "Build a terminal UI (TUI) for Forge: a live, keyboard-driven dashboard over the "
     "daemon's unix-socket HTTP API showing the priority queue, running attempts WITH "
     "their live progress (the attempt_progress fields — running turns, phase, last-event "
     "age), recent tasks and states, the budget/usage windows, and a tail of the journal. "
     "Keybindings to select+inspect a task (its detail) and to cancel one. Use an "
     "established Go TUI library — bubbletea (charmbracelet) OR tview — pick ONE and pin "
     "its exact version via go get (proxy.golang.org is allowed in the sandbox). Structure "
     "it as a new package internal/tui invoked by a new `forge tui` subcommand under "
     "cmd/forge. Reuse the existing client/protocol/view types over the socket; respect "
     "the repo's import-boundary rules (the `boundary` linter in `just check`). Handle the "
     "daemon being down gracefully. Make `just check` pass. integrate off; branch only."),
    ("docs", "opus", 55, 70, "modularization plan",
     "Produce a concrete modularization PLAN for the forge repo at docs/MODULARIZATION.md "
     "(write only the doc — do NOT modify code). Operator's direction: break the codebase "
     "into modules — core, tools, tui, web — with TUI and web as submodules. Decide and "
     "justify the MECHANISM (a Go multi-module go.work workspace with a go.mod per module, "
     "vs. plain internal packages, vs. nested submodules) with trade-offs for a codebase "
     "that today ships one binary (daemon + CLI + worker) from one module. Define the "
     "module boundaries and the ACYCLIC dependency direction (what `core` exports; what "
     "`tools`/`tui`/`web` may import). Map EVERY current top-level package to its target "
     "module — internal/controlplane (13k LOC: how does it split across core/web/tools?), "
     "worker, store, model, protocol, tools, modes, plugin, integrator, mcpserve, kb, "
     "stats, eval, doctor, logging, and cmd/forge. Give an INCREMENTAL migration sequence "
     "where `just check` stays green at each step, and call out the risks (the existing "
     "`boundary` linter, import cycles, the plugin wire contract, CI). Survey the code "
     "before proposing — ground every claim in what's actually there. integrate off; "
     "branch only."),
]


def main():
    print("== overnight round 2: tests / TUI / modularization plan ==")
    for mode, model, prio, turns, title, prompt in TASKS:
        body = {
            "prompt": prompt, "repositories": ["forge"], "mode": mode, "model": model,
            "autonomy": "auto", "class": "normal", "priority": prio, "max_turns": turns,
            "timeout_seconds": 5400, "integrate": False, "title": f"overnight: {title}",
        }
        st, resp = call("POST", "/api/v1/tasks", body)
        print(f"  [{mode:9} {model} p{prio}] {title:24} -> {st}")
        if st >= 400:
            print("     ERROR:", resp[:200])


if __name__ == "__main__":
    main()
