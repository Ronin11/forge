#!/usr/bin/env python3
"""Greenfield stress test: 10 takes on equitizr's core idea.

Distills equitizr down to its through-line — "help people see who really owns
the everyday local businesses around them, with a confidence score and a
sourced evidence chain" — and greenfields ten distinct product angles on it,
each a complete, offline, demo-ready web app. This is a deliberate token burn
to (a) produce client-ready demos and (b) surface Forge's own rough edges at
scale. Every task runs mode=greenfield (worktree is an empty dir Forge owns;
the finished project moves to <projects_root>/<slug>), integrate=false.

Run:  python3 scripts/greenfield-equitizr-demo.py         # submit all 10
      python3 scripts/greenfield-equitizr-demo.py 2 5 7   # submit a subset (1-based)
"""
import json
import socket
import sys

SOCK = "/home/ronin/.forge/forge.sock"

# Shared brief prepended to every angle: the core idea + the hard constraints
# that make each build a real, self-contained demo rather than a broken shell.
PREAMBLE = (
    "Build a complete, demo-ready web app around this core idea: HELP PEOPLE SEE "
    "WHO REALLY OWNS THE EVERYDAY LOCAL BUSINESSES AROUND THEM — surfacing hidden "
    "ownership, especially private-equity roll-ups, with a CONFIDENCE SCORE and a "
    "TRANSPARENT, SOURCED EVIDENCE CHAIN: storefront -> consumer brand -> the "
    "owning firm -> the firm's SEC / regulatory registration -> the funds and SPVs "
    "it raises capital through, every link carrying its own source (type + URL) and "
    "an as-of date. There is no single public registry of 'this storefront is "
    "PE-owned,' so the value is the triangulation and showing your work.\n\n"
    "HARD CONSTRAINTS (this is a demo you could hand a client):\n"
    "- Runs fully OFFLINE with NO external API keys and no paid services. Ship a "
    "curated seed dataset embedded in the repo: ~40 real PE / holding firms, ~90 "
    "consumer-facing brands they own (restaurants, dental/vet/eye-care roll-ups, "
    "home services, car washes, gyms, retail), ownership EDGES each with a source "
    "type (verified / reported / scraped), a source URL, an as-of date, a stake "
    "type (majority / minority / franchisor), and a confidence weight; plus ~150 "
    "sample storefronts across a few US metros with lat/lng so the app has real "
    "texture. Curate from well-documented public knowledge; mark anything "
    "uncertain as 'reported' and lower its confidence rather than inventing "
    "precision.\n"
    "- Preserve the CONFIDENCE MODEL: a 0-100 score = name-match quality x "
    "ownership sourcing (verified 1.0 / reported 0.85 / scraped 0.7) x stake type "
    "(majority 1.0 / franchisor 0.95 / minority 0.75); drop matches below 55. "
    "Never show a claim without a way to drill into its evidence.\n"
    "- Prefer a STATIC, self-contained front end (no backend or secrets required to "
    "run the demo) with the dataset as bundled JSON; a small build step is fine but "
    "keep dependencies light. If you show a map, Leaflet is fine. Make it polished "
    "and genuinely presentable — real typography, considered layout, works on "
    "mobile.\n"
    "- Be honest about limits: include a short, visible methodology note explaining "
    "the data is a curated demo sample and how confidence is derived.\n\n"
    "THIS VARIATION:\n"
)

ANGLES = [
    ("pe-near-me",
     "The flagship consumer map, executed beautifully. A clean, mobile-first map of "
     "a metro; tap any storefront to see its confidence score and expand the full "
     "evidence chain inline. Filter by category and by confidence threshold. The "
     "single best-crafted faithful take on the core idea — this is the one you'd "
     "open first in a pitch."),
    ("owner-lookup",
     "Flip the map for a SEARCH-FIRST decision tool: a big search box ('who owns "
     "___?'). Type a brand or business and get an instant verdict card — 'Owned by "
     "Roark Capital, 84% confidence' — with the money trail beneath it. Framed for "
     "the moment before you spend: fast, decisive, one answer per query, shareable "
     "result URLs."),
    ("keep-it-local",
     "Awareness -> ACTION. Same map, but every PE-owned storefront surfaces nearby "
     "INDEPENDENT / locally-owned alternatives in the same category ('PE-owned "
     "coffee here; three independent cafes within 0.5mi'). The dataset must include "
     "a set of independent businesses to recommend. The product is a swap engine, "
     "not just a scold."),
    ("neighborhood-report-card",
     "An ANALYTICS dashboard, not a map. Enter a ZIP or pick a neighborhood and get "
     "a report: '% of dining that is PE-owned,' concentration by category, the top "
     "firms operating locally, and a simple trend line over recent years (seed a few "
     "years of as-of dates to make the trend real). Journalistic, chart-forward, "
     "screenshot-ready stats."),
    ("follow-the-money",
     "Lead with the ENTITY GRAPH itself: an interactive, force-directed graph of "
     "firm -> funds/SPVs -> brands -> local storefronts, every edge clickable to its "
     "source and as-of date. Built for researchers and journalists who want to trace "
     "and export a chain. 'Show your work' is the entire product."),
    ("rollup-tracker",
     "A CATEGORY deep-dive on the private-equity roll-up. Pick an industry (dental, "
     "veterinary, car washes, gyms, eye care) and see how it got consolidated: a "
     "timeline of acquisitions, which firms dominate, a market-concentration read, "
     "and the brands now under each firm. Tells the roll-up STORY with data behind "
     "each beat."),
    ("ownership-card",
     "A shareable OWNERSHIP-CARD generator. Any brand produces a beautiful, "
     "self-contained card — owner, fund, confidence, top sources, a confidence "
     "meter — designed to be screenshotted and shared (think an og-image you'd post). "
     "Gallery of cards on the landing page; deep-link to any single card. Education "
     "through virality."),
    ("transparency-portal",
     "A public-interest CIVIC data tool, trust-grade. A searchable, filterable, "
     "sortable registry of every ownership claim with full provenance, a prominent "
     "METHODOLOGY page, a confidence-scoring explainer, and a 'download the data' "
     "export (CSV/JSON). Sober, credible, accessible (WCAG-minded) — the version a "
     "newsroom or nonprofit would stand behind."),
    ("guess-the-owner",
     "A GAMIFIED daily quiz that makes the data sticky. 'Is Chuy's owned by private "
     "equity? Guess.' Reveal the answer with the full evidence chain and confidence; "
     "keep a streak and a score; a shareable result. Five to ten rounds a day drawn "
     "from the dataset. Learning disguised as a game."),
    ("main-street-watch",
     "An ADVOCACY dashboard for a small-business owner or local organizer: "
     "independents vs. PE in a chosen market, a watchlist of firms and categories, a "
     "'concentration alert' framing when one firm owns a lot of a category locally, "
     "and talking points generated from the data. B2B / civic-advocacy angle on the "
     "same core."),
]


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


def task(slug, angle):
    return {
        "prompt": PREAMBLE + angle + (
            f"\n\nSuggested project slug: '{slug}'. Report it as project_name so the "
            "finished app lands in its own directory."),
        "repositories": ["greenfield"],
        "mode": "greenfield",
        "model": "opus",
        "autonomy": "auto",
        "class": "normal",
        "priority": 60,
        "max_turns": 120,
        "timeout_seconds": 5400,
        "integrate": False,
        "title": f"greenfield: {slug}",
    }


def main():
    pick = [int(a) for a in sys.argv[1:]] if len(sys.argv) > 1 else None
    print("== greenfield equitizr demo — submitting ==")
    for i, (slug, angle) in enumerate(ANGLES, start=1):
        if pick and i not in pick:
            continue
        st, body = call("POST", "/api/v1/tasks", task(slug, angle))
        wid = ""
        try:
            wid = json.loads(body).get("work", {}).get("id", "")[:8]
        except Exception:
            pass
        print(f"  {i:2}. {slug:24} -> {st}  {wid}")
        if st >= 400:
            print("      ERROR:", body[:300])


if __name__ == "__main__":
    main()
