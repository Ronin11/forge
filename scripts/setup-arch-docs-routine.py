#!/usr/bin/env python3
"""Create the `architecture-docs` routine: a reusable, {{repo}}-templated docs-mode
task that produces or refreshes docs/ARCHITECTURE.md — a mermaid-first, high-level
walkthrough of any codebase plus an honest "state of the codebase" section.

Invoke against any registered repo:
  forge task add --routine architecture-docs --repo <name> --integrate
or run the routine directly. Re-running refreshes the existing doc.

  python3 scripts/setup-arch-docs-routine.py
"""
import json
import socket
import sys

SOCK = "/home/ronin/.forge/forge.sock"

PROMPT = """\
Produce or refresh a high-level ARCHITECTURE walkthrough for {{repo}} at
docs/ARCHITECTURE.md — the document a new engineer reads FIRST to grok the whole
system before diving into detailed design docs. It is a map, not an encyclopedia.

Approach:
1. Survey broadly before writing. Establish: the top-level packages/modules and
   their responsibilities and rough sizes; the entry points; the data model
   (schema / migrations / core types); the key runtime components and how they
   communicate (processes, transports, storage, external services); the external
   interfaces; and the build/test setup. Never name a component that does not
   exist — verify against the code.
2. If docs/ARCHITECTURE.md already exists, READ it and REFRESH it: preserve
   still-accurate content, update what changed, do not discard good prose.
3. Write docs/ARCHITECTURE.md, mermaid-first, containing:
   - One paragraph: what this system is, and its core design tenets.
   - A COMPONENT MAP (mermaid flowchart/graph) of the major components and how
     they communicate — processes, transports, storage, external services.
   - The primary LIFECYCLE / STATE MACHINE (mermaid stateDiagram) if the system
     has one (a job / request / entity lifecycle).
   - A key end-to-end REQUEST or DATA FLOW (mermaid sequenceDiagram) for the most
     important path.
   - 2–4 compact diagrams for the other load-bearing subsystems, each with a short
     prose walkthrough.
   - A "where things live" table mapping each major concern to its package/module.
   - A short "STATE OF THE CODEBASE" section: size/complexity hotspots, obvious
     fragility or tech-debt, missing tests, and 3–8 RANKED "things to work on or
     think about" — each concrete and grounded in what you actually found (cite
     file:line where useful). Be honest, not flattering.
4. Keep every mermaid diagram valid and small — split any diagram past ~15 nodes.
   Keep the doc skimmable: diagrams + short prose, never walls of text.
5. Cross-link to the existing detailed docs rather than duplicating them.

Do not modify code. Commit docs/ARCHITECTURE.md. Declare and run any docs checks
the repository defines.
"""


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


ROUTINE = {
    "name": "architecture-docs", "mode": "docs", "model": "sonnet", "prompt": PROMPT,
    "repositories": ["forge"], "budget_class": "normal", "autonomy": "auto",
    "max_turns": 60, "timeout_seconds": 3600, "require_sandbox": False,
    "integrate": False, "concurrency": 1, "priority": 50, "max_questions": 0,
}


def main():
    st, body = call("POST", "/api/v1/routines", ROUTINE)
    if st == 409:  # exists — replace it
        call("DELETE", "/api/v1/routines/architecture-docs")
        st, body = call("POST", "/api/v1/routines", ROUTINE)
    print(f"architecture-docs ({ROUTINE['mode']} / {ROUTINE['model']}) -> {st}")
    if st >= 400:
        print("  ERROR:", body[:400]); sys.exit(1)


if __name__ == "__main__":
    main()
