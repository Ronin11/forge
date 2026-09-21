# The customer portal

*2026-09-17. A project, seen from the customer's side.*

The operator's web client shows Forge as a factory: tasks, attempts,
verdicts, costs. The customer never wants any of that. They want to know
what is running for them, what is being built, what needs them, and what
got done. That page is the portal, and it is the concierge's other face:
the same three kinds of message, with a screen instead of a text thread.

## What they see

One page per project, in their words, with no ids, branches, verdicts or
logs anywhere on it.

- **Running for you.** Each deploy target: its name, where it runs, when
  it was last deployed, whether the last check and look passed, and the
  last look's screenshot. Alongside it, each run workflow: its name and
  its last three jobs, newest first — when each ran, whether it went ok,
  failed, or needs a person, and, on failure, a one-line reason. This is
  the list of automations Forge built and keeps working for them, and
  the recent record of the ones that run on their own schedule or
  trigger rather than sitting behind a URL.
- **Being built.** Every open initiative, newest first: its outcome
  sentence, a plain state (in progress, waiting on you), and how many
  pieces of work it's taken so far. The last ten, and an "and n more"
  line past that — never the whole backlog on one page.
- **Needs you.** Every open question addressed to them, answerable in
  place. This is the human rung, on a screen.
- **Done.** Landed work, newest first, one line each: a landed
  initiative is its outcome sentence and how many pieces of work it
  took; a landed task that belongs to no initiative is its title, if it
  was filed in their own words, or a line cut from the first sentence
  of the request, any file path, extension or line number stripped, at
  most 120 characters on a word boundary — never the operator's own
  instructions. The last ten, and an "and n more" line past that.
- **Ask.** A box. What they type goes through the concierge exactly as a
  text would: a request becomes a task, a question gets an answer, a
  need starts an interview whose questions then appear under Needs you.
- **Your plan.** The brief they confirmed and the backlog cut from it,
  so they can see what they said and what is queued.

## Times

The kernel and the record speak UTC only; the customer reads their own
clock. Every moment on the page — a deploy target's last update, a job's
start, a landed line's shipping date, and when a question was asked —
goes through one tag, `<time data-ts="<unix seconds>">`, whose text is
the UTC fallback ("Nov 14, 2023, 22:13 UTC") for a viewer without
JavaScript. A small inline script (`portal/src/local-time.js`) replaces
that text with the same shape in the viewer's zone ("Nov 14, 2023, 17:13
EST"), keeping any wording in front of it (`data-prefix`, "Shipped ").
The server never learns the viewer's zone. `portal/tests/local_time.rs`
snapshots the script under fixed zones.

## What it is

A third client on the client contract (`docs/CLIENT.md`), beside the
TUI and the operator's web page: its own crate, `portal/`, a small
server like `forge-web` that reads through the CLI's JSON, scoped to one
project per link. It writes through two verbs only: `forge answer` for
Needs you, and `forge ask` for the box.

Access is a per-project link carrying a token (`/p/<token>`), minted
with `forge project portal <name>` and stored on the project; a token
opens one project and nothing else. Accounts, several projects per
customer, and an email digest come later, when there is more than one
customer.

It binds to loopback like the operator's page and sits behind the same
reverse proxy the deploy targets use, on the operator's host, so a
customer reaches it at a name and nothing on the box is exposed that
was not already.

## Reachable

The reverse proxy entry is a name pointed at `forge-portal`'s loopback
port, the same shape as every deploy target's Caddy block (see
docs/DEPLOY.md, "Provisioning"):

```
portal.example.com {
    reverse_proxy 127.0.0.1:7799
}
```

`forge-portal` itself runs as a user-level systemd unit,
`docs/ops/forge-portal.service`, kept alive the same way any long-running
service on the operator's host is.

The link still has to reach the customer. The Signal plugin
(`plugins/signal/`) sends it two ways, both `forge project portal
<project>` under the hood: a contact configured in its `CONTACTS` table
gets their link back by texting `/portal`, and gets it unprompted the
first time `forge intake accept` creates a project for them (the
`project_created` event, docs/CLIENT.md) — the moment there is finally
something on the page worth looking at. Its `PORTAL_URL` setting is the
name from the Caddyfile block above, so the link it sends is the one
that resolves.

## What it is not

Not the operator's page with things hidden: a separate crate with its
own copy, so nothing operator-only can leak through a forgotten filter.
Not a chat: the box is one message in, and the reply comes back as a
line under Done, Being built, or Needs you.

## Build order

1. `forge project portal <name>`: the token, its table, and the project
   scoping of the JSON the portal reads.
2. The crate and the read-only page: Running for you, Being built, Done,
   Your plan.
3. Needs you with the answer form, and Ask through the concierge.
4. Reachable: the proxy entry on the operator's host
   (docs/DEPLOY.md, "Provisioning"), the `forge-portal` systemd unit
   (`docs/ops/forge-portal.service`), and the Signal plugin sending a
   contact their link on `/portal` and on their first project.
