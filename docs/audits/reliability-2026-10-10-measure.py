#!/usr/bin/python3 -I
"""Pass one of the reliability audit: per-subsystem, per-day outcome counts
from the store, the event log and the worker journal, with silent-failure
days flagged. Read-only."""
import collections
import datetime as dt
import json
import re
import sqlite3
import sys

import os

HOME = os.environ["FORGE_HOME"]  # the operator's data directory; no default, so the test naming rule holds
JOURNAL = sys.argv[1]  # journalctl --user -u forge-worker --since "30 days ago" -o short-iso > journal.txt
DAYS = 30
TODAY = dt.date(2026, 10, 10)
FIRST = TODAY - dt.timedelta(days=DAYS - 1)


def day(ts):
    return dt.datetime.utcfromtimestamp(int(ts)).date()


def in_window(d):
    return FIRST <= d <= TODAY


# series[(subsystem, name)][day] = [runs, ok]
series = collections.defaultdict(lambda: collections.defaultdict(lambda: [0, 0]))


def add(sub, name, d, ok):
    if not in_window(d):
        return
    c = series[(sub, name)][d]
    c[0] += 1
    c[1] += 1 if ok else 0


db = sqlite3.connect(f"file:{HOME}/forge.db?mode=ro", uri=True)
db.row_factory = sqlite3.Row

task_project = {}
for r in db.execute("select id, project, repo from tasks"):
    task_project[r["id"]] = r["project"] or (r["repo"] or "").rstrip("/").split("/")[-1]

# A. tasks by terminal state, per project
for r in db.execute("select id, state, finished_at from tasks where finished_at is not null"):
    ok = r["state"] == "succeeded"
    add("task", f"{task_project.get(r['id'], '?')}", day(r["finished_at"]), ok)
    add("task", "all", day(r["finished_at"]), ok)

# B. steps
for r in db.execute("select task_id, step, state, started_at, provider from attempts where finished_at is not null"):
    ok = r["state"] == "succeeded"
    add("step", r["step"], day(r["started_at"]), ok)
    if r["step"] == "supervisor":
        add("supervisor", "run", day(r["started_at"]), ok)

# E. jobs
for r in db.execute("select workflow, state, finished_at from jobs where finished_at is not null"):
    add("job", r["workflow"], day(r["finished_at"]), r["state"] == "ok")

# F. deploys
for r in db.execute("select project, target, finished_at, check_ok, smoke_ok, rolled_back_to from deploys where finished_at is not null"):
    ok = (r["check_ok"] in (1, None)) and (r["smoke_ok"] in (1, None)) and r["rolled_back_to"] is None
    add("deploy", f"{r['project']}/{r['target']}", day(r["finished_at"]), ok)

# G. provider probes
for r in db.execute("select provider, at, ok from provider_probes"):
    add("provider-probe", r["provider"], day(r["at"]), bool(r["ok"]))

# J. messages per channel/direction
for r in db.execute("select channel, direction, at from messages"):
    add("message", f"{r['channel']}:{r['direction']}", day(r["at"]), True)

# K. worktree removals vs finishes
for r in db.execute("select worktree_removed_at from tasks where worktree_removed_at is not null"):
    add("gc", "worktree-removed", day(r["worktree_removed_at"]), True)

# C/D/L: events
sup_pat = re.compile(r"supervisor (could not read|reading the record)")
with open(f"{HOME}/events.jsonl") as fh:
    for line in fh:
        try:
            e = json.loads(line)
        except Exception:
            continue
        d = day(e.get("ts", 0))
        t = e.get("type")
        if t == "check":
            proj = task_project.get(e.get("task"), "?")
            add("check", f"{e['name']}", d, e.get("ok", False))
            add("check-by-project", f"{proj}:{e['name']}", d, e.get("ok", False))
        elif t == "op":
            add("op", e.get("name", "?"), d, e.get("ok", False))
        elif t == "note":
            m = sup_pat.search(e.get("text", ""))
            if m:
                add("supervisor", "note", d, m.group(1) == "reading the record")
        elif t == "provider_held":
            add("provider-held", e.get("provider", "?"), d, False)
        elif t == "job_finished":
            pass

# H/I: journal
restart_pat = re.compile(r"systemd\[\d+\]: Started ")
refused_pat = re.compile(r'egress: refused "CONNECT ([^:"]+):')
with open(JOURNAL, errors="replace") as fh:
    for line in fh:
        if len(line) < 11:
            continue
        try:
            d = dt.date.fromisoformat(line[:10])
        except ValueError:
            continue
        if restart_pat.search(line):
            add("worker", "restart", d, False)
        m = refused_pat.search(line)
        if m:
            add("egress-refused", m.group(1), d, False)

# Report
days = [FIRST + dt.timedelta(days=i) for i in range(DAYS)]
out = []
out.append(f"# Reliability audit, pass one: outcomes per day, {FIRST} to {TODAY}\n")
out.append("Each cell is ok/runs for that day; `-` is no runs. A `!` marks a day with 3+ runs and zero successes.\n")
findings = []
for (sub, name), per in sorted(series.items()):
    total = sum(v[0] for v in per.values())
    oks = sum(v[1] for v in per.values())
    if total < 3:
        continue
    row = []
    flagged = []
    for d in days:
        if d in per:
            n, k = per[d]
            cell = f"{k}/{n}"
            if n >= 3 and k == 0 and sub not in ("worker", "egress-refused", "provider-held", "message", "gc"):
                cell += "!"
                flagged.append(d)
            row.append(cell)
        else:
            row.append("-")
    # streaks of flagged days
    streak = []
    best = []
    prev = None
    for d in flagged:
        if prev and (d - prev).days == 1:
            streak.append(d)
        else:
            streak = [d]
        if len(streak) > len(best):
            best = list(streak)
        prev = d
    last3 = [per[d] for d in days[-3:] if d in per]
    l3n = sum(v[0] for v in last3)
    l3k = sum(v[1] for v in last3)
    out.append(f"\n## {sub} / {name}  ({oks}/{total} ok over the window; last 3 days {l3k}/{l3n})\n")
    out.append("| " + " | ".join(d.strftime("%m-%d") for d in days) + " |")
    out.append("|" + "---|" * DAYS)
    out.append("| " + " | ".join(row) + " |")
    if len(best) >= 2:
        findings.append((sub, name, best[0], best[-1], len(best), total, oks))

out.insert(2, "\n## Findings: two or more consecutive days of 3+ runs with zero successes\n")
out.insert(3, "| subsystem | name | from | to | days | runs in window | ok |")
out.insert(4, "|---|---|---|---|---|---|---|")
for i, f in enumerate(sorted(findings, key=lambda f: -f[4])):
    out.insert(5 + i, f"| {f[0]} | {f[1]} | {f[2]} | {f[3]} | {f[4]} | {f[5]} | {f[6]} |")
print("\n".join(out))
