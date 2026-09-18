#!/bin/bash
# the investigate step: reads README.md and the task it was given, and
# when they contradict, stops with a question quoting both instead of
# planning around it
prompt="$(cat)"
rule="$(grep -m1 '.' README.md)"
task="$(printf '%s\n' "$prompt" | awk '/^Task:$/{getline; print; exit}')"
question="README.md says: \"$rule\" The task asks: \"$task\" These contradict; which wins?"

esc() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

question_json="$(esc "$question")"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"blocked","needs_input":{"tried":"read README.md and the task; they contradict","question":"'"$question_json"'","options":[],"context":"","checkpoint":null},"changes":[],"checks_run":[],"claims":[]}}'
