# Forge as a service: go-to-market for the operated model

*2026-09-14. A working document for one decision: whether to sell what
Forge produces to small businesses, and what it would take.*

## The offer

Automations built and kept working for a business that has no
engineering team, for a monthly fee. A customer tells us, in plain
words, what they want done: the invoice PDFs that land in a mailbox
turned into rows in their accounting system; a nightly report from
three tools into one message; a form on their site that files a ticket
and texts the owner. We build it, prove it works against checks we
write, deploy it, watch it, and fix it when their tools change. They
see every request, what was built, and what it cost. They talk to a
person when a decision is theirs to make.

This is Tier A of the two shapes considered in the architecture
discussion: we operate Forge; customers are clients of its output.
They never author workflows, never run agents, never see a repository
unless they ask to.

## Who it is for

A business of five to fifty people with a recurring digital chore that
one person currently does by hand or has glued together with a
no-code tool that keeps breaking. Property managers, clinics, trades
firms, agencies, e-commerce shops on one platform. The buyer is the
owner or the operations lead; the user is whoever owns "the
spreadsheet". They have accounts on a handful of SaaS tools with APIs
they have never seen, and no one to call.

Not for: anyone with an engineering team (they will run Forge
themselves), anyone whose automation is their product, regulated data
until the isolation work below is done.

## What they get

- **Intake.** A portal and an email address. A request in plain words
  becomes a task; a vague one comes back as a question within the hour,
  from the investigator or from us.
- **A build that is verified, not vouched for.** Every automation ships
  with checks we write from the request, run by the kernel, not by the
  agent that built it. The customer sees "verified" and what that meant.
- **Deployment and watching.** The automation runs on infrastructure we
  hold; failures open a task automatically; a change in their tool's
  API is our problem.
- **A record.** Every task, attempt, decision and cost, in their portal,
  forever: the exact prompt each agent was given, every check that ran
  and its output, every question and who answered it. This is the answer
  to the obvious objection. "No human reviewed this" is a hard sell to
  an auditor; "no human reviewed it, and here is every gate it passed
  and every decision anyone made about it" is a different conversation,
  and one most of this industry cannot have, because their record stops
  at the pull request.
- **A human.** For decisions only they can make, and for the first
  conversation. The supervisor and the investigator take the rest.

## How it works underneath

One repository per customer, owned by us: their automations, the
checks that prove them, and a `.forge/` config with their budget and
their protected paths. Forge runs on our machines. Agents run on API
keys per customer, in a sandbox that holds only that customer's tree
and secrets. Landing opens a pull request that we or the kernel merge,
and a deploy step runs the automation where it lives. The supervisor
answers the questions the record can settle; we answer the rest,
through the same `forge answer` the record keeps.

Nothing here is a new system. It is the factory as it runs today, with
a customer at the intake and a deploy at the landing.

## What this week's numbers say

Forge has spent the last week building itself, which is a harder
workload than a small business's automations: larger codebase, stricter
checks, one agent refactoring the tool that runs it.

| | |
|---|---|
| Tasks in the last seven days | 94 |
| Landed on main | 47 |
| Mean cost per landed task | $1.48 |
| Range per landed task | $0.17 to $4.20 |
| Total agent spend | $124.80 |
| Supervisor rulings | 8, at $0.39 each |
| Questions answered without a human | 6 of 6 rulings that answered |
| Mean coding attempt | 202 seconds, 28 tool calls |

Half the tasks that did not land were measurement twins run without
landing on purpose, and retries whose successor landed; the honest
number is that nearly every piece of work landed, most on the first
task, at one to two dollars. The supervisor has not yet needed a human
for a question it took.

For a customer's automation, a piece of work is smaller and the checks
are simpler, so the cost per piece should sit at the low end of that
range. The number to price against is cost per landed piece of work,
which the kernel already measures per workflow.

## Pricing shape

A base subscription that includes a number of pieces of work per month
and unlimited running of what is built, with pieces beyond that at a
per-piece price. Three tiers by volume, not by features.

| | Agent cost per piece | Price per piece | Included pieces | Base per month |
|---|---|---|---|---|
| Starter | ~$2 | $40 | 5 | $200 |
| Standard | ~$2 | $30 | 15 | $450 |
| Scale | ~$2 | $25 | 40 | $1000 |

The margin is not in the agent cost, which is small; it is in the
human time per customer, which the supervisor and the investigator are
built to drive down. The first three months exist to measure that time.
If a customer costs more than an hour of a person per month at
Standard, the price is wrong or the intake is.

## What has to be built

In order. The first two are the ones that decide whether this is safe
to sell; the rest are plumbing on tenancy that already exists.

1. **Isolation that is a boundary. The trigger is the first task filed
   from text a stranger wrote, not the first customer.** The documented
   2026 attack pattern (Clinejection, RoguePilot, poisoned agent config
   files) is one sentence: an agent holding elevated credentials while
   reading untrusted input. Forge is already better than that in one respect and no better
   in another. An attempt's clone has no remote, its home directory is
   an empty tmpfs, and the kernel does the push, so the agent holds no
   git credentials and cannot land anything the checks did not pass.
   But it has unrestricted network egress and the operator's model
   token, so a hostile issue body can make it send the repository, or
   the token, anywhere; and the only thing against that is a sentence
   in a prompt telling the model that repository text is data. Each task must run in its own microVM or
   container with only that customer's tree and secrets, egress limited
   to what the task declares, and credentials scoped to the task and
   expired when it ends. This touches `sandbox.rs` and the launch path
   only; the kernel already treats the sandbox as a wrapper around one
   command. **Until it lands, the github-issues plugin stays disabled**:
   it is the piece that arms the pattern.
2. **Agents on API keys, metered per customer.** The claude CLI on a
   subscription cannot be resold and its rate windows are shared. The
   agent runs on the API with a key per customer; the per-attempt cost
   the store already records becomes the invoice line. The runner
   behind `agent::run` grows a second backend; the contracts do not
   change (this is also the second-runner item in LATER.md).
3. **Remote repositories and landing by pull request.** A GitHub App
   with per-installation tokens; clone from the remote; `landing.rs`
   opens a pull request instead of fast-forwarding, with auto-merge as
   a per-customer choice. The integrator's merge-and-verify is
   unchanged.
4. **A deploy operation.** A built-in operation after landing that runs
   the customer's automation where it lives (a scheduled job, a
   webhook), with its own check. Operations already exist; this is one
   more action file and a host to run it on.
5. **Digital twins of the tools a customer's automation talks to.** The
   verification moat, and the piece this plan did not have. A customer's
   automation is mostly integration: their accounting system, their
   mailbox, their ticket tracker. Checks that hit the real services are
   slow, rate limited, and cannot be made to fail on demand, so neither
   the agent nor the hidden suite can iterate against them. The answer,
   taken from StrongDM: generate self-contained behavioural clones of
   those APIs from their public documentation, run them locally, and
   point the checks at the clone. Thousands of runs an hour, no keys,
   and failure modes you can ask for. Without this the automations are
   verified shallowly, which is the one thing this business cannot
   afford.

6. **The customer portal.** Accounts (sign-in with Google or GitHub),
   membership in a customer, operator and viewer roles, per-customer
   tokens, TLS. Built on the client contract (`docs/CLIENT.md`) and the
   client crate landing this week, as a third client beside the TUI and
   the operator's web page. Requests, tasks, decisions and costs, and
   `forge answer` as a button for the questions that are theirs.
7. **Secrets.** A per-customer store, injected into that customer's
   sandbox as environment, never into a prompt, never into a log.
8. **Fairness and quotas.** The queue claims oldest-first across every
   repository; it needs round-robin across customers and a concurrency
   cap per customer. Budgets per task and per day already exist.
9. **Retention and deletion.** Attempt logs hold customer code. A
   retention policy, backups, and deletion on churn.
10. **Intake by email**, so the first request never needs a login.

Rough sizes: items 1 and 2 are two weeks each and cannot be skipped;
3 and 4 a week together; 5 is open-ended and pays for itself per
integration; 6 two weeks on the client crate; 7 through 10 a week
together.

## Risks, plainly

- **Prompt injection, which is no longer hypothetical.** A customer's
  data (an email, a PDF, a web page, an issue body) reaches the agent as
  untrusted content, and 2026 has in-the-wild cases of exactly this
  chain: a payload in an issue title compromising a coding tool's
  published package, hidden comments making an agent exfiltrate its
  token. The isolation work bounds the blast radius to one customer;
  nothing bounds it further. Do not take regulated data, and do not open
  external intake, until item 1 is done and reviewed.
- **Our evidence is the favourable case.** Everything Forge has built is
  greenfield and agent-authored: itself, and a game grown from a seed.
  The independent research is consistent that autonomy works best on
  exactly that and struggles on mature, high-constraint codebases, which
  is where a customer's existing systems live. One operator, two
  repositories, three weeks. Treat the numbers above as a floor on what
  is possible and as no evidence at all about brownfield.
- **The human rung.** If the supervisor answers fewer questions in
  customer work than in our own, the model above breaks on support
  time. This is the first thing the design partners measure.
- **Model and provider dependence.** One vendor, one CLI, one runner
  today. Item 2 makes the runner pluggable; the contracts already are.
- **Cost variance.** A piece that hits the turn cap three times costs
  ten times the mean. The task budget stops the bleeding; the early
  ending and the retry-from-branch rule reduce it; the per-piece price
  must absorb the tail.
- **Scoping.** "An automation" has to mean something bounded. The
  investigate directive is the tool for this: a request that is
  impossible or unbounded comes back as a question before any money is
  spent.
- **Churn.** Small businesses churn. The automation keeps running after
  they leave only if it is theirs; the repository model makes that
  possible and the offer should say so.

## What to measure from day one

Cost per landed piece of work per customer; the share of questions
answered by the supervisor versus a person; minutes of human time per
customer per month; time from request to landed; customer-visible
failures per month; and supervisor rulings that a later task showed to
be wrong. All but the human minutes already exist in the store and the
decisions table.

One more, which the independent research calls the tell and which we do
not yet compute: **defect escape**, the share of landed work that a
later task had to fix. Throughput without it is the number every
optimistic vendor report leads with and every independent study
distrusts. It is derivable from what the store already holds, and it is
the honest counterweight to a clean landing rate.

## The first ninety days

- **Weeks 1 to 4.** Items 1 and 2. In parallel, three design partners
  at a token price, intake by email, requests entered by hand, every
  hour of human time logged. The point is the human-minutes number, not
  the revenue.
- **Weeks 5 to 8.** Items 3 through 6. Partners move from email to the
  portal. The pricing table is redone from the partners' numbers.
- **Weeks 9 to 13.** Items 7 through 9. Open to ten customers at the
  real price. Decide, on the numbers, whether the operated model holds
  or whether the platform (Tier B) is the business.

## What this is not

Not a platform where customers run their own agents. Not a code tool
for developers. Not a promise that the agent is right: the promise is
that nothing lands unverified and that a person is one question away.
