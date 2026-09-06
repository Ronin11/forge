# Running Forge for the Family

*How cheaply forge could serve a few more humans, and what stands between
here and there. 2026-09-05.*

## The short answer

**$0/month hosting, ~$5–15/person/month in tokens, about two afternoons of
work.** Forge itself is one Go binary and a SQLite file; the machine it
already runs on is the cheapest cloud there is. The entire real cost is
model tokens, and the capacity/budget machinery to cap them per-person
already exists.

## Hosting: the box you have

| Option | Cost | Verdict |
|---|---|---|
| Homelab + Tailscale | $0 | The answer. No public exposure; family reaches forge through channels (Signal/email), never the API. |
| Hetzner ARM (4 vCPU/8GB) | ~€7/mo | If the homelab ever can't be on 24/7. Any Linux box with bwrap works. |
| Per-person VPS instances | ~€4/mo each | Cleaner isolation, more ops, loses the shared brain. Not worth it at family scale. |

The daemon is single-token and the UI is operator-grade: **never on the
public internet**. Family members don't need it — they live entirely in the
channel layer, which is a feature, not a workaround.

## Tokens: the entire cost structure

- **The operator's subscription** stays the operator's: personal licensing,
  and family traffic would eat the rate windows the learning loop feeds on.
- **Family traffic rides an API-billed runner** (`billing = "api"`). Light
  use — a few haiku/sonnet tasks a day, escalation ladder capped at sonnet —
  lands around **$5–15/person/month**. The caps are already real code:
  daily USD cap, budget classes, and the API-dollars pool pattern from the
  learning ledger, reusable per sender.
- **The expensive machinery** (benchmarks, reflection, big-model
  escalation) stays on the operator's subscription or off for family work.
  A local-model runner can absorb trivial tasks at ~$0, quality-gated by
  the router's existing Wilson gate.

## The architecture is accidentally close

Forge is single-operator today, but the pieces lean multi-tenant already:

- **Assistant sessions key by sender** — each family member gets their own
  persistent conversation thread with referent memory ("how'd it go?"
  resolves to *their* task) for free.
- **`submitted_by` attributes every work** — per-person history and spend
  attribution have a column waiting.
- **Channels are the whole interface** — Signal and email plugins exist;
  family members text forge like a person, and that is the entire product
  surface they ever see.
- **Approvals stay with the operator** — the Human Queue, purchase gates,
  and proposals remain one person's job. Family asks; the operator's
  guardrails decide.

## The gaps (small, known)

1. **Multi-recipient signal plugin** — config takes one `recipient` today;
   a `recipients` list plus routing replies to the asking sender is an
   afternoon.
2. **Per-sender budget caps** — the learning-pool pattern (rolling-window
   spend query + refusal with a journaled reason) pointed at
   `submitted_by`, with family work pinned to the API runner.
3. **Family repos** — forge is repo-centric, so family "stuff" lives in
   git: a repo per person or shared ones (recipes, trip plans, the school
   newsletter). Quietly wonderful; needs nothing but `git init`.

## Why bother

Beyond the fun: multi-sender, per-user budgets, channel-only interaction
**is the product shape**. Family members are design partners who complain
honestly and churn loudly. The first paying user of forge-as-a-service will
want exactly what the second phone number forces us to build.

## First step (costs nothing)

Add one family member's number to the signal plugin and watch a real second
sender exercise the session keying. The first gap that actually bites sets
the build order.
