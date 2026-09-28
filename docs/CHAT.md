# Ask Forge

Ask Forge is a conversation with Forge about its own state: "why did 903
fail five times", "what is initiative 56 waiting on", "how much did last
night cost", "file a task that does X on nucleosynthesis". It is the
successor of Forge 1's ask channel. `forge ask <project> <message>` is a
different thing (the customer front door, [INTAKE.md](INTAKE.md)); this is
the operator talking to Forge, from the CLI (`forge chat`) and from the
web client (the Chat page), over the same sessions.

The model reads; it does not act. It answers from a fixed set of tools that
are deterministic code over the store and the CLI's own JSON, and the three
tools that would change something only record a *proposal* the operator
confirms before anything runs.

## Talking

```sh
forge chat "why did task 903 fail five times"      # opens session 1
forge chat --session 1 "and what did it cost"      # continues it
forge chat sessions                                # list them, newest first
forge chat show 1                                  # the transcript, tool calls and cost
forge chat confirm 12.0                            # run a proposed action
forge chat reject 12.0                             # or drop it
```

`forge chat [--session <id>] [--provider <name>] [--json | --stream]
<message>` says something and prints the reply. With `--json` it prints one
object (`session`, `turn`, `reply`, `cost_usd`, `tool_calls`, `proposals`);
with `--stream` one JSON event per line as they happen (the session, each
tool call with its result, the reply), which is what the web client reads.
A message that begins with `-` is passed after `--`.

The web client's `/chat` page is the same thing: sessions on the left, the
transcript on the right, a box at the bottom. Tool calls appear as the
model makes them and the reply when it lands; a proposal is drawn with
**confirm** and **reject** buttons. See [CLIENT.md](CLIENT.md) for the
routes. The web layer keeps no conversation and runs nothing itself: it
calls `forge chat` verbs.

## The model

The model runs on the host through the **chat runner**: any provider whose
`runner` is `chat` (an OpenAI-compatible `/chat/completions` endpoint, see
[JOBS.md](JOBS.md)); the first such provider by name, or the one `--provider`
names. When none is configured it uses the operator's default provider (the
`plan` role's), which must be able to run a step with no tools — `claude-cli`
can, `codex-cli` and `copilot-cli` refuse.

```toml
[providers.openai-chat]
runner = "chat"
base_url = "https://api.openai.com/v1"
model = "gpt-5-mini"
api_key_env = "OPENAI_API_KEY"
price_usd_per_million_input = 0.25
price_usd_per_million_output = 2.00
```

Each *step* is one bounded launch with no tools of its own. The model
answers with one JSON object:

```json
{"reply": "Looking at task 903.", "tool": "task", "arguments": {"id": 903}}
```

`tool` names one of the tools below and `arguments` its arguments; Forge
runs it and launches the next step with the result appended. An empty
`tool` finishes, and `reply` is the answer. A message may make up to eight
rounds of calls; if it has not answered by then, it says what it found. A
provider that ignores the schema is still read: plain words are a finished
reply, and `tool_calls`/`name`/`args` spellings are accepted. The
conversation so far (cut from the front past 24 KB) and this message's tool
results are the step's prompt; the system message is the `chat` directive
followed by the tool list.

## The system prompt

The system prompt is the `chat` directive in the library
([EXECUTION.md](EXECUTION.md)): a built-in action like `concierge`, so it is
versioned like every other prompt. Its text is `prompt` in
`src/builtins/actions/chat.toml`; an operator overrides it by putting their
own `actions/chat.toml` in the workflows catalog (which shadows the
built-in, with a `prompt_file` and fragments if they like). Every
assistant turn records the hash of the system text it ran under
(`chat_turns.prompt_hash`), so a change of behavior can be traced to a
change of prompt. The tool list appended to it is rendered from the code
that dispatches the tools, so the model is never told about a tool that does
not exist.

## The tools

Read tools answer from the record. Each takes a JSON object; every result
is redacted (below) and bounded to 12 KB, strings cut and lists capped
rather than the whole thing refused.

| Tool | What it returns |
|---|---|
| `task {id}` | The task (state, reason, workflow, provider, budget, lineage, plan), every attempt with the checks that failed and their first lines, its operations, the kernel's diagnosis, its assessment. `forge trace --json`, whitelisted. |
| `attempt {task, attempt_no}` | One attempt whole: verdict rows, the agent's summary, claims and changes, tokens. |
| `tasks {state?, project?, initiative?, grep?, workflow?, before?, limit?}` | The queue, newest first (`forge log`). |
| `initiative {id}` / `initiatives {project?}` | An initiative's outcome, state and hold, each lineage's latest task, its questions and rulings (`InitiativeDoc`); or every initiative with counts by state. |
| `decisions {task?, grep?, limit?}` | Recorded answers and rulings (`forge decisions`). |
| `requests {}` | Every blocked task and what it waits on (`forge requests`). |
| `doctor {}` | The health checks that are not OK, the rest by name (`forge doctor`). |
| `stats {days?}` | Spend in the last 24 hours against `per_day_usd`, daily landings and cost, per-workflow and per-role figures — from the store alone. |
| `log_tail {task, attempt_no?, lines?}` | The last lines of an attempt's log. |
| `events_since {cursor?, since_ts?, task?, limit?}` | The event log after a cursor or a unix time; `next` continues. |
| `add_task {task, project \| initiative, workflow?, after?}` | **Proposes** filing a task. |
| `answer_question {task, answer}` | **Proposes** answering a blocked task's question. |
| `retry_task {task, workflow?, again?}` | **Proposes** retrying a finished task as a new one. |

Anything else — a shell, `git`, `read_file` — is an error the model reads
("no tool named ..."); there is no fallback.

## The confirm gate

A write tool never acts. Calling it checks the arguments against the record
(the task exists and is blocked, the project lists a repository, the
workflow exists, the task is finished) and records a *proposal* on the
assistant turn: a sentence saying what confirming would do, with its
arguments in normal form. `add_task` files only into a repository a project
lists: the model does not choose paths.

The proposal has an id, `<turn>.<position>` (`12.0`), and a status:
`proposed` until the operator decides.

- **confirm** (`forge chat confirm 12.0`, or the button) claims the proposal
  by compare-and-swapping the stored calls, so two confirmations of one
  action race and exactly one wins; runs it once through the same queue
  code `forge add`, `forge answer` and `forge retry` use; and records the
  outcome on the proposal (`confirmed`, or `failed` with the error — a
  failed action is not run again) and as an `action` turn in the session.
  Filing or retrying also leaves a row in `forge decisions` with kind
  `chat-action`, answered by `operator`, citing `chat session N`; an answer
  is recorded by the queue's own answer path with the same citation.
- **reject** records `rejected`; nothing runs.

A decided action cannot be decided again. The model sees the decisions in
the conversation on its next turn.

## What is recorded

Every turn is a row in `chat_turns`: session, role (`user`, `assistant`,
`action`), text, `tool_calls` (each call with its arguments and result, or
its error, or its proposal and where it stands), `cost_usd`, provider,
model, `prompt_hash`, time. `chat_sessions` holds the title (the first
message) and when it was last active. `forge chat show` and the Chat page
render them; nothing is only in memory.

## Rules

- **No host git, no shell.** The tools are Rust functions over the store and
  the CLI's documents. None spawns a process; the model's own launch has no
  tools and runs in an empty scratch directory.
- **No filesystem reads** beyond the event log and the attempt logs under
  `FORGE_HOME/logs` that `forge trace` already names. `log_tail` takes the
  path from the store and refuses any that does not resolve inside the logs
  directory.
- **No secrets in tool results.** Every result, and the model's reply, goes
  through one redactor (`src/chat/redact.rs`) before it reaches the model,
  the record, or the screen. It masks the values of the projects' secrets,
  the providers' environment and API keys, and the web token wherever they
  appear; anything shaped like a credential (`sk-…`, `ghp_…`, `AKIA…`, a
  JWT, the word after `Bearer`, a URL's password, `key=value` and JSON
  pairs whose key names a secret); and the value under any JSON key that
  names one (`password`, `api_key`, `access_token` — but not
  `input_tokens`). Tool results are also whitelists, not dumps: a task's
  worktree path and workflow text are not offered.
- **Cost counts against the per-day budget.** Every step's cost — tokens at
  the provider's prices, or the CLI's own figure — is on its turn and is
  part of `Store::spent_since`, the sum `per_day_usd` is checked against and
  `forge doctor` reports as `spend`. At the cap, `forge chat` refuses ("daily
  budget reached") like `forge run` does; a message that fails half way
  still records what it spent.
- A provider held for a refused login or a spent rate window is not asked.
- Everything the model reads in a result is data: the directive says so,
  as every prompt does ([SECURITY.md](../SECURITY.md)), and the only text
  that can ask for anything is the operator's own messages. Even a model
  that is persuaded can only *propose*.

## Testing

The unit tests are beside the code: `src/chat/tools.rs` (dispatch: the
catalog and the dispatcher agree, an unknown tool is refused, results are
whitelisted, bounded and redacted, `log_tail` stays under the logs
directory, write tools change nothing), `src/chat/actions.rs` (the confirm
gate: nothing runs before confirm, a confirmed action runs exactly once, a
rejected or failed one never runs, only a proposal can be decided, racing
decisions have one winner), `src/chat/redact.rs`, `src/chat/step.rs` (how
the model's answers are read). `tests/e2e/chat.rs` runs the real binary
against a fake chat provider — a loopback `/chat/completions` endpoint the
test plays the model from: a question about a seeded failed task is
answered from the `task` tool's result; a request to file work is proposed,
files nothing, and is filed on `forge chat confirm`; the per-day budget
stops a second message. `web/tests/chat_server.rs` and `chat.test.js` cover
the routes and the page's rendering.
