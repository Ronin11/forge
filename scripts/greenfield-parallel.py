#!/usr/bin/env python3
"""Fire the remaining 7 equitizr greenfield angles IN PARALLEL.

Reuses PREAMBLE + ANGLES from greenfield-equitizr-demo.py, but targets one
dedicated fake repo per angle (distinct path -> distinct path-lease) so they run
concurrently instead of serializing on the shared virtual `greenfield` repo.
mode=greenfield still applies (keeps the phase structure); the move-into-place
no-ops because the repo is not the virtual greenfield origin — the app is built
and committed in the repo itself. 150-turn ceiling.

Run:  python3 scripts/greenfield-parallel.py
"""
import importlib.util
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("gdemo", os.path.join(HERE, "greenfield-equitizr-demo.py"))
gdemo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gdemo)
PREAMBLE, ANGLES, call = gdemo.PREAMBLE, gdemo.ANGLES, gdemo.call

# The 7 not yet built (indices 3..9 in the shared ANGLES list). Each slug is also
# its registered repo name.
REMAINING = ANGLES[3:]


def task(slug, angle):
    return {
        "prompt": PREAMBLE + angle + (
            "\n\nThis repository is empty except for a README — build the app at its "
            f"root and commit as you go. Suggested project slug: '{slug}'."),
        "repositories": [slug],
        "mode": "greenfield",
        "model": "opus",
        "autonomy": "auto",
        "class": "normal",
        "priority": 60,
        "max_turns": 150,
        "timeout_seconds": 6000,
        "integrate": False,
        "title": f"greenfield//: {slug}",
    }


def main():
    print("== parallel greenfield — submitting 7 ==")
    for slug, angle in REMAINING:
        st, body = call("POST", "/api/v1/tasks", task(slug, angle))
        wid = ""
        try:
            wid = json.loads(body).get("work", {}).get("id", "")[:8]
        except Exception:
            pass
        print(f"  {slug:24} -> {st}  {wid}")
        if st >= 400:
            print("     ERROR:", body[:300])


if __name__ == "__main__":
    main()
