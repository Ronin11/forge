---
size: L
model: sonnet
autonomy: auto
---
Rebuild equitizr from scratch in this empty repository: a complete,
demo-ready web app that HELPS PEOPLE SEE WHO REALLY OWNS THE EVERYDAY LOCAL
BUSINESSES AROUND THEM — surfacing hidden ownership, especially
private-equity roll-ups, with a CONFIDENCE SCORE and a TRANSPARENT, SOURCED
EVIDENCE CHAIN: storefront → consumer brand → the owning firm → the firm's
SEC / regulatory registration → the funds and SPVs it raises capital
through, every link carrying its own source (type + URL) and an as-of date.
There is no single public registry of "this storefront is PE-owned," so the
value is the triangulation and showing your work.

HARD SEQUENCING RULE (three benches have now shipped with an empty
[checks] table): the FIRST task of the plan must wire forge.toml's [checks]
with at least a build/typecheck command that runs green on the scaffold,
and every later task keeps it green and extends it. A final product whose
[checks] is empty scores as a failure regardless of anything else.

HARD CONSTRAINTS (this is a demo you could hand a client):

- Runs fully OFFLINE with NO external API keys and no paid services. Ship a
  curated seed dataset embedded in the repo: ~40 real PE / holding firms,
  ~90 consumer-facing brands they own (restaurants, dental/vet/eye-care
  roll-ups, home services, car washes, gyms, retail), ownership EDGES each
  with a source type (verified / reported / scraped), a source URL, an as-of
  date, a stake type (majority / minority / franchisor), and a confidence
  weight; plus ~150 sample storefronts across a few US metros with lat/lng
  so the app has real texture. Curate from well-documented public knowledge;
  mark anything uncertain as "reported" and lower its confidence rather than
  inventing precision.
- Preserve the CONFIDENCE MODEL: a 0-100 score = name-match quality ×
  ownership sourcing (verified 1.0 / reported 0.85 / scraped 0.7) × stake
  type (majority 1.0 / franchisor 0.95 / minority 0.75); drop matches below
  55. Never show a claim without a way to drill into its evidence.
- Prefer a STATIC, self-contained front end (no backend or secrets required
  to run the demo) with the dataset as bundled JSON; a small build step is
  fine but keep dependencies light. If you show a map, Leaflet is fine. Make
  it polished and genuinely presentable — real typography, considered
  layout, works on mobile.
- Be honest about limits: include a short, visible methodology note
  explaining the data is a curated demo sample and how confidence is
  derived.
- Declare the repository's checks in a forge.toml at the root (build + any
  tests) and keep them green.

Deliver a searchable storefront/brand lookup, a firm drill-down with the
full evidence chain, and a map view. The bar is "works, looks professional,
and every ownership claim is inspectable" — not feature count.
