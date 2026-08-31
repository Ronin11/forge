#!/usr/bin/env python3
"""Create the role routines and role-pipeline workflows on the local daemon.

Reproducible setup for the "route work through roles" experiment: eight role
routines (a triage front door, a challenge gate, PM, architect, programmer,
reviewer, QA, and an evaluator) plus four flows of different depth/order. Each
role commits a deliverable under flow/ so the next role — stacked on it — reads
it; every flow runs integrate=false (thrown away) and ends in r-eval, the
objective measurement. Routines take an {{objective}} the run supplies (or self-
direct when absent). Run:  python3 scripts/setup-role-flows.py
"""
import json
import socket
import sys

SOCK = "/home/ronin/.forge/forge.sock"
DEFAULT_REPO = "equitizr"


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
    status = int(head.split(b" ")[1])
    return status, body_bytes.decode(errors="replace")


def routine(name, mode, prompt, model="sonnet", max_turns=40, integrate=False):
    return {
        "name": name, "mode": mode, "model": model, "prompt": prompt,
        "repositories": [DEFAULT_REPO], "budget_class": "normal", "autonomy": "auto",
        "max_turns": max_turns, "timeout_seconds": 2400, "require_sandbox": False,
        "integrate": integrate, "concurrency": 1, "priority": 50, "max_questions": 0,
    }


ROUTINES = [
    routine("r-triage", "run",
        "You are the triage front door for {{repo}}. A request has come in:\n\n{{objective}}\n\n"
        "First, challenge it — 'but why though?' What is the REAL underlying problem or goal, and is this request the right solution to it? If it's obviously low-value or misguided, say so plainly with your reasoning. Then, if it's worth doing, SIZE it: small (a localized change), medium (a feature touching a few areas), or large (cross-cutting, needs design + atomization). Recommend exactly one flow to run: flow-quick (small), flow-standard (medium), flow-deep (large), or flow-design-first (large, design-led). Write your verdict, the real problem, the size, and the recommended flow to flow/triage.md, commit it, and run the repository's checks. Do not implement anything."),
    routine("r-challenge", "run",
        "You are the challenge gate for {{repo}}. The proposed work is:\n\n{{objective}}\n\n"
        "Before anyone builds anything, ask 'but why though?'. Interrogate the premise: what problem does this really solve, for whom, and is it worth the cost? Is there a simpler or better approach, or a reason NOT to do it at all? Give a clear verdict — PROCEED or RECONSIDER — with your reasoning, and if proceeding, one sharpened sentence stating the actual goal. Write this to flow/00-challenge.md, commit it, and run the checks. Do not implement anything."),
    routine("r-pm", "run",
        "You are the product manager for {{repo}}. Read flow/00-challenge.md if present for the sharpened goal. The work is:\n\n{{objective}}\n\n"
        "Produce a build plan: restate the goal in one line, define what is in scope and explicitly out, write acceptance criteria a verifier can check, and atomize the work into ordered, independently-shippable tasks — the bigger the work, the more meticulously atomized, each task small enough to implement and verify on its own. Surface open questions. Write the plan to flow/10-plan.md, commit it, and run the checks. Do not implement."),
    routine("r-architect", "run",
        "You are the architect for {{repo}}. Read any prior flow/ notes (challenge, plan). The work is:\n\n{{objective}}\n\n"
        "Design the approach: the components and their boundaries, the key technical decisions and their trade-offs, the risks and mitigations, and a concrete step-by-step implementation plan someone else could follow. Read the code you would touch. Write the design to flow/20-design.md, commit it, and run the checks. Do not implement."),
    routine("r-programmer", "implement",
        "You are the implementer for {{repo}}. Read the prior flow/ notes (plan, design) and implement the work:\n\n{{objective}}\n\n"
        "Follow the plan and the repository's conventions; work in small commits; add tests for the behavior you change; make the declared checks pass. If the plan is wrong or incomplete, note it briefly and do the sensible thing. Do not modify the flow/ notes.", max_turns=60),
    routine("r-reviewer", "run",
        "You are the reviewer for {{repo}}. The work implemented is:\n\n{{objective}}\n\n"
        "Review the changes on this branch (git diff against the base) against the plan (flow/10-plan.md if present) and the repository's conventions: correctness, edge cases, security, performance, and completeness. Write findings ranked by severity — each with a file:line and a concrete fix — plus an overall verdict (ship / needs-work), to flow/30-review.md. Commit it and run the checks. Do not modify the implementation."),
    routine("r-qa", "run",
        "You are QA for {{repo}}. The work implemented is:\n\n{{objective}}\n\n"
        "Test the change against the acceptance criteria in flow/10-plan.md (if present): exercise the happy path and edge cases, look for regressions, and record every defect with steps to reproduce and expected-vs-actual, plus a pass/fail verdict, to flow/40-qa.md. Commit it and run the checks. Do not fix — report."),
    routine("r-eval", "run",
        "You are the evaluator for {{repo}} — the objective measurement at the end of the flow. The original request was:\n\n{{objective}}\n\n"
        "Read the entire flow/ trail (challenge, plan, design, review, qa — whichever exist) and the actual changes (git diff against the base). Judge the OUTCOME, not the effort: did the flow solve the real problem the challenge identified? Rate correctness, completeness, code quality, and whether the effort matched the work's size, each 1-5, then give an overall score 1-5 with a one-paragraph justification and the single biggest weakness. Write this to flow/99-eval.md, commit it, and run the checks."),
]


def step(name, routine_name, after=None, stack=True):
    s = {"name": name, "routine": routine_name}
    if after:
        s["after"] = [{"step": after, "on": "success", "stack_on": stack}]
    return s


# Each flow gates everything on the challenge and ends in the evaluator; every
# step stacks on the previous one so each role reads the prior role's commit.
def chain(name, roles):
    steps, prev = [], None
    for r in roles:
        sname = r[2:]  # r-pm -> pm
        steps.append(step(sname, r, after=prev))
        prev = sname
    return {"name": name, "steps": steps}


WORKFLOWS = [
    chain("flow-quick",        ["r-challenge", "r-programmer", "r-reviewer", "r-eval"]),
    chain("flow-standard",     ["r-challenge", "r-pm", "r-programmer", "r-reviewer", "r-eval"]),
    chain("flow-deep",         ["r-challenge", "r-pm", "r-architect", "r-programmer", "r-reviewer", "r-qa", "r-eval"]),
    chain("flow-design-first", ["r-challenge", "r-architect", "r-pm", "r-programmer", "r-reviewer", "r-eval"]),
]


def main():
    print("== routines ==")
    for rt in ROUTINES:
        st, body = call("POST", "/api/v1/routines", rt)
        if st == 409:  # exists — replace it
            call("DELETE", f"/api/v1/routines/{rt['name']}")
            st, body = call("POST", "/api/v1/routines", rt)
        print(f"  {rt['name']:14} {rt['mode']:10} -> {st}")
    print("== workflows ==")
    for wf in WORKFLOWS:
        st, body = call("POST", "/api/v1/workflows", wf)
        if st == 409:
            call("DELETE", f"/api/v1/workflows/{wf['name']}")
            st, body = call("POST", "/api/v1/workflows", wf)
        steps = " -> ".join(s["name"] for s in wf["steps"])
        print(f"  {wf['name']:18} [{steps}] -> {st}")
        if st >= 400:
            print("    ERROR:", body[:300])
            sys.exit(1)


if __name__ == "__main__":
    main()
