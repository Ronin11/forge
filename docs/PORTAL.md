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
  last look's screenshot. This is the list of automations Forge built
  and keeps working for them.
- **Being built.** Each open initiative as its outcome sentence and a
  plain state: in progress, waiting on you, done. Never a task count.
- **Needs you.** Every open question addressed to them, answerable in
  place. This is the human rung, on a screen.
- **Done.** Landed work, one line each in the words of the request, with
  when it went live.
- **Ask.** A box. What they type goes through the concierge exactly as a
  text would: a request becomes a task, a question gets an answer, a
  need starts an interview whose questions then appear under Needs you.
- **Your plan.** The brief they confirmed and the backlog cut from it,
  so they can see what they said and what is queued.

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
4. The proxy entry on the operator's host and a link sent over the
   customer's channel.
