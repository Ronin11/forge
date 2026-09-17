# Intake

*2026-09-15. How a person who does not think in workflows tells Forge
what to build, and how Forge finds out what they mean before building.*

Forge's front door today is `forge add <repo> <text>`, written by an
operator who knows what a task is. The people the operated model is for
do not. One runs a business over text messages and has five workflows he
would not call workflows. Asked "what do you need automated", he cannot
answer; asked "what did you do with the last three texts you got", he
can, at length. Intake is the part of Forge that has that second
conversation and turns it into a brief a project can be built from.

## Principles

**Instances, never requirements.** The interviewer asks for the last
three, the most annoying one, the one that went wrong. It never asks
for a list of requirements, a priority, or a definition. The workflows
appear as repetition across instances, and the interviewer's job is to
notice the repetition and name it back: "so every time a customer sends
a photo of the job, you save it, text them a quote, and write it in the
book. Is that right?" The naming is the work; the person only has to say
yes, no, or "except when".

**The product is a brief in the person's words.** Not a specification.
For each workflow it names: what starts it, what comes in, what goes
out, who else is involved, what goes wrong today, what "working" would
look like to them, and what must not change. Plus, for the whole brief:
where it needs to run (their phone, a laptop, somewhere else), and a
list of things they said they do not want touched. Every sentence in it
was confirmed by the person before anything is built, so the first task
is never a surprise.

**On the channel they already use.** The interview happens where the
person already is. For the text-message business that is Signal, and
the Signal plugin already carries intake. Email and a portal are the
same interviewer on other channels.

**One question per message, in plain words.** Never two questions,
never jargon, never a form. The person is doing this between customers.

**The interviewer decides nothing.** It does not choose what to build,
does not promise, does not estimate, and does not persuade. It asks,
reflects, and writes down. The person and the operator decide.

## Mechanics: the question rung, reused

An interview is an intake task, and each of its turns is a question the
task blocks on. That is machinery Forge already has: a blocked task with
a question, `forge answer` to resume it, the answer recorded as a
decision. Intake adds one field to a question, **who it is addressed
to**, so the channel plugin knows to deliver it to the person rather
than to the operator, and returns their reply as the answer. Days
between turns cost nothing; the task waits.

The `intake` workflow is one directive, `interview`, on a read-only
contract like `investigate`: it is given the brief so far and every
answer, asks the next question or, when the checklist is satisfied,
writes the brief and stops with a final question, the confirmation, to
the person. The brief lives on the task the way a plan does (`t.plan`),
and is shown in full to each turn.

The checklist that ends an interview, per workflow named: trigger,
inputs, outputs, other people, failure today, success signal, and the
do-not-touch list; for the brief: where it runs and the confirmation.
The interviewer may not stop early and may not add items the person did
not say.

When the person confirms, the operator sees the brief (it is on the
record, and the notify plugin can say so). `forge intake accept <task>`
creates or updates the project: its purpose from the brief's first
paragraph, its backlog from the workflows, the deploy target from
"where it runs" as a draft the operator completes. The first initiative
is then filed the ordinary way, which is where the planned workflow's
investigate step takes over with the brief as its context.

## What the person sees

Their own words, reflected. A question at a time. A summary they can say
no to. And afterwards, on the same channel, the things the notify
plugin already says: "the quote-by-text automation is live", or "we hit
a question only you can answer", with the question. They never see a
task id, a branch, or a verdict.

## Guardrails

Everything the person sends is data. The interviewer's prompt says so
the way every Forge prompt does, and the interviewer has no tools that
act: it reads and writes the brief, nothing else. A per-interview budget
and a cap on questions per day, so a confused conversation costs a
bounded amount and never floods someone's phone. The operator can step
into any interview with `forge answer` on the same task, and can
withdraw it. The person can stop at any time by saying so, which the
interviewer must recognise and honour.

## What is not built

No personas, no debate between agents about what the person "really"
needs, no product manager, no persuasion, and no attempt to build
before the brief is confirmed. The interviewer is one contract with a
checklist. If the brief is wrong the fix is a better checklist or a
better question, both of which are files.

## Build order

1. The addressee on a question: a `to` field on the blocked question and
   on the decision that answers it; the Signal plugin delivers to that
   contact and answers on their behalf.
2. The `interview` directive and `intake` workflow, with the brief as a
   structured document and the checklist as its schema; a fake person
   in the e2e suite who answers from a script.
3. `forge intake accept`, creating the project, backlog and draft
   target.
4. Email as a second channel.

The first interview is the one with the business that runs on text,
with the operator watching every turn.

## The front door is not the interview (added 2026-09-17)

A customer sends three kinds of message, and only one wants an
interview.

- **A request**: "make the quote text say 'usually same day'". They have
  said exactly what they want. It becomes a task on their project with
  the brief as its reason, the checks verify it, the deploy target ships
  it, and the same thread says it is live. A one-off is a task of one.
- **A question**: "did the reminder go to the Hendersons?" No build. It
  is answered from the project's record, the way the supervisor answers
  from ours.
- **A need**: "I keep losing track of who I've quoted." A symptom with a
  workflow underneath. This is the interview's case, and only this.

So the front door is the **concierge**: a read-only directive that sees
the message, the project's brief and backlog, and its recent tasks, and
decides which of the three it is, asking one question only when it
genuinely cannot tell. The interview is one branch; a task is another;
an answer is the third. `forge ask <project> <message>` is the verb, and
the Signal plugin routes a contact's message through it instead of
filing it blindly as a task.

**Mechanics.** `concierge` is a directive on the `plan` contract, read
only like `interview` and `investigate` (src/builtins/actions/concierge.toml).
Its workflow is `setup` then `concierge` alone — no code step, nothing to
land. It is given the message, the project's purpose, its brief if it has
one (the confirmed plan of its intake task, read the way `forge intake
accept` reads it), its backlog, its deploy targets, and its last twenty
tasks, each as id, state, and first line. It returns one JSON decision as
its plan: `kind` is `request`, `question`, `need`, or `unclear`, with one
more field for whichever kind it chose — `task` for a request (the
customer's words, tidied, with the reason), `answer` for a question
(drawn only from what it was given, never guessed), `reason` for a need
(one sentence saying why an interview is warranted), or `question` for
unclear (the one question that would settle it). It never stops with its
own `needs_input`: a decision is one turn, not a conversation.

`forge ask <project> <message> [--from <contact>]` runs `concierge` to
completion (the same way `forge run` drives a task) and acts on the
decision:

- **request**: files a task on the project — its first repository, its
  own default workflow (else `direct`) — with the decided text, and the
  customer's own message as its `title` (see docs/PORTAL.md, "Done").
- **question**: prints the answer and records a `decisions` row
  (`answered_by` "concierge", `answered_for` the `--from` contact),
  hung off the concierge's own task; no task is filed.
- **need**: files an intake task, the message as its text (with `Contact:
  <name>.` appended when `--from` names one, the convention an intake
  task's text already carries), so the interview picks the contact up
  the way it always has.
- **unclear**: files a small placeholder task, already blocked, with
  `needs input: <question>` as its reason and `--from` as the question's
  `to`, so it shows in `forge requests` and the channel plugin can
  deliver it and answer on the contact's behalf exactly like an
  interview's question.

Every decision is on the record: a `concierge_json` column, raw JSON, on
the task it produced (the filed request, the intake task, or the blocked
placeholder), or, for a question, the `decisions` row itself.

**The escalator.** Because Forge keeps a record, one-offs turn into
automation without anyone asking for it. After the third request of the
same shape on a project, the concierge says so: "that is the third time
you have asked me to change a quote by hand; want it to pull the price
from your sheet?" That is the interviewer's naming-of-repetition applied
across weeks instead of within one conversation. A yes files an
initiative from the pattern.

What keeps this small: the customer sees texts, one question at a time,
from a stock model with a short prompt. No conversation engine, no state
beyond a blocked task and its answer, no personas. The structure is two
artifacts, a brief and a backlog. If it ever feels like a form, the fix
is the prompt, not more harness.
