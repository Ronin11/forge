---
size: L
model: sonnet
autonomy: auto
max_turns: 50
timeout: 5400
---
Build "directory-harvest" from scratch in this empty repository: a
demo-ready scraping toolkit that TURNS A MESSY, INCONSISTENT PILE OF
BUSINESS-DIRECTORY WEB PAGES INTO CLEAN, TRUSTWORTHY STRUCTURED RECORDS —
the kind of thing a city economic-development office would use to rebuild
its "who operates where" list from a legacy directory site that was
hand-edited for fifteen years and never had a schema. The value is not the
fetch; it is that every record that comes out is right, every record that
could not be extracted is REPORTED rather than silently dropped or
invented, and a reviewer can trace any field back to the exact page and
element it came from.

HARD SEQUENCING RULE (three benches have now shipped with an empty
[checks] table): the FIRST task of the plan must wire forge.toml's [checks]
with at least a build/typecheck command that runs green on the scaffold,
AND a `setup` command (e.g. `setup = ["npm", "ci"]`) that makes a fresh
clone check-ready — the merge gate runs checks in a clone that has never
installed anything, so without setup every merge dies on missing
dependencies. Every later task keeps both green and extends the checks. A final product whose
[checks] is empty scores as a failure regardless of anything else.

HARD CONSTRAINTS (this is a demo you could hand a client):

- HERMETIC. This repository is built and checked in a sandbox with NO
  NETWORK EGRESS. Nothing may be fetched from the internet, not the
  target pages and not packages: assume anything not already installed
  cannot be installed, so prefer the standard library of whichever
  mainstream language you choose (Python's html.parser / csv / json or
  Go's encoding + strings are plenty) and fall back to it if a
  third-party parser is unavailable. The scraper reads pages FROM DISK
  (a directory of .html files); there is no HTTP client in the critical
  path, and no check may depend on a socket.
- THE CORPUS IS BUNDLED, AND YOU GENERATE IT FIRST. Ship a deterministic
  fixture generator (seeded RNG, seed recorded in the code) that writes
  a fictional business directory of at least 40 HTML pages into
  fixtures/: a paginated listing index, per-business detail pages, and
  at least three visibly different page templates (the site "redesigned"
  twice). Alongside the HTML the generator writes fixtures/manifest.json —
  the ground truth: every business it planted, with every field value,
  which page(s) it lives on, and the expected outcome for pages it
  deliberately corrupted. Commit both the generator and its output.
- THE CORPUS MUST BE HOSTILE, ON PURPOSE. The generator plants, and the
  manifest labels, at least: unclosed and mis-nested tags; missing
  optional fields (no phone, no hours, no website); the same business
  listed twice under slightly different names (dedupe by a documented
  rule and record which pages merged); phone numbers and hours in three
  or more surface formats that must normalise to one canonical form;
  HTML entities and non-ASCII names (an accented owner, an em-dash in a
  street name); a page truncated mid-record; a page that is a "404 Not
  Found" body served with a directory-looking wrapper; a page of pure
  junk; and at least one page where a field is present but AMBIGUOUS
  (two candidate addresses) so the honest answer is "unresolved", not a
  guess.
- STRUCTURED OUTPUT WITH A DOCUMENTED SCHEMA. Write out/records.json and
  out/records.csv (same records, same order) and SCHEMA.md describing
  every column: type, canonical format, nullability, and what "unknown"
  looks like. Every record carries provenance — source file(s) and a
  short locator for where each non-null field came from — plus an
  extraction confidence and a list of flags (normalised_phone,
  merged_duplicate, missing_hours, ambiguous_address, …). Also write
  out/errors.json: one entry per page or record the parser could not
  handle, saying what went wrong. Unparseable input is a first-class
  result, never an exception that kills the run and never a record
  padded with plausible-looking defaults.
- ACCURACY IS A CHECK, NOT A CLAIM. Declare the repository's checks in a
  forge.toml [checks] table at the root and keep them green. At minimum:
  a check that regenerates fixtures/ from the seed and fails if any file
  differs from what is committed; the unit tests for the parser and the
  normalisers; and an accuracy script that runs the scraper end to end
  and asserts, against fixtures/manifest.json: the exact expected record
  count after dedupe; per-field exact-match accuracy of at least 98%
  across all planted fields on parseable pages; ZERO fabricated values
  (no non-null field that the manifest says was absent); every corrupted
  page appearing in errors.json with the expected classification; and a
  handful of named spot checks (a specific business's normalised phone,
  the merged duplicate resolving to one record, the ambiguous address
  reported as unresolved). The check prints the accuracy table so a
  reviewer can read it from the log.
- Provide one obvious entry point (a CLI: `scrape <fixtures-dir> <out-dir>`
  or equivalent) and a README that shows the whole loop in under a
  minute: generate, scrape, check, and where to look when a page fails.
  Include a short, visible methodology note: how confidence is derived,
  the dedupe rule, and what the tool deliberately refuses to guess.

Deliver the generator, the resilient parser + normalisers, the
schema-documented outputs with provenance, the error report, and the
wired accuracy checks. The bar is "every record is right or honestly
marked, and a script proves it" — not page count or parser cleverness.
