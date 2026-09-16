#!/bin/bash
# Shared frames for the fake agents in this directory. Source this file,
# then call the functions below instead of hand-writing the same JSON.

# result <summary> [path:kind ...]
# Prints a successful result frame with the structured envelope: schema_version
# 1, needs_input null, empty checks_run and claims, and the given summary and
# changes.
result() {
  local summary="$1"
  shift
  local changes="[]"
  if [ "$#" -gt 0 ]; then
    changes="["
    local sep=""
    local entry path kind
    for entry in "$@"; do
      path="${entry%%:*}"
      kind="${entry##*:}"
      changes="$changes$sep{\"path\":\"$path\",\"kind\":\"$kind\"}"
      sep=","
    done
    changes="$changes]"
  fi
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$summary"'","needs_input":null,"changes":'"$changes"',"checks_run":[],"claims":[]}}'
}

# session <id>
# Prints the system init frame announcing the given session id.
session() {
  echo '{"type":"system","subtype":"init","session_id":"'"$1"'"}'
}

# parse_resume <args...>
# Sets $resume to the value following a --resume argument, or "" if absent.
parse_resume() {
  resume=""
  while [ $# -gt 0 ]; do
    if [ "$1" = "--resume" ]; then
      resume="$2"
    fi
    shift
  done
}

# codex_thread <thread_id>
# Prints codex's thread.started frame: agent.rs reads this as the session id.
codex_thread() {
  echo '{"type":"thread.started","thread_id":"'"$1"'"}'
}

# codex_command <id> <command>
# Prints the item.started/item.completed pair codex emits around a shell
# command, as agent.rs's codex parser (and the early-ending Watch it feeds)
# expects them.
codex_command() {
  local id="$1" cmd="$2"
  echo '{"type":"item.started","item":{"id":"'"$id"'","type":"command_execution","command":"'"$cmd"'"}}'
  echo '{"type":"item.completed","item":{"id":"'"$id"'","type":"command_execution","command":"'"$cmd"'","exit_code":0}}'
}

# codex_error <id> <message>
# Prints an item.completed error item.
codex_error() {
  echo '{"type":"item.completed","item":{"id":"'"$1"'","type":"error","message":"'"$2"'"}}'
}

# codex_result <summary> [path:kind ...]
# Prints the final agent_message item whose text is the structured envelope
# codex would write when given --output-schema: the same shape `result`
# gives the claude fakes, schema_version 1, needs_input null, empty
# checks_run and claims, and the given summary and changes.
codex_result() {
  local summary="$1"
  shift
  local changes="[]"
  if [ "$#" -gt 0 ]; then
    changes="["
    local sep=""
    local entry path kind
    for entry in "$@"; do
      path="${entry%%:*}"
      kind="${entry##*:}"
      changes="$changes$sep{\"path\":\"$path\",\"kind\":\"$kind\"}"
      sep=","
    done
    changes="$changes]"
  fi
  local text='{"schema_version":1,"summary":"'"$summary"'","needs_input":null,"changes":'"$changes"',"checks_run":[],"claims":[]}'
  local escaped="${text//\\/\\\\}"
  escaped="${escaped//\"/\\\"}"
  echo '{"type":"item.completed","item":{"id":"result","type":"agent_message","text":"'"$escaped"'"}}'
}

# codex_usage <input> <cached> <output> <reasoning>
# Prints the turn.completed usage frame; its four counts sum into the
# attempt's token counts.
codex_usage() {
  echo '{"type":"turn.completed","usage":{"input_tokens":'"$1"',"cached_input_tokens":'"$2"',"output_tokens":'"$3"',"reasoning_output_tokens":'"$4"'}}'
}

# argv_debug <argv...>
# Prints a line of a type agent.rs's parsers do not otherwise handle (so it
# is only ever logged, never acted on) carrying the fake's own argv as a
# JSON array. Forge writes the log itself, from the child's stdout pipe, so
# this reaches the test even when the fake ran sandboxed and could not
# write a file of its own back out to the host.
argv_debug() {
  local out="[" sep="" a esc
  for a in "$@"; do
    esc="${a//\\/\\\\}"
    esc="${esc//\"/\\\"}"
    out="$out$sep\"$esc\""
    sep=","
  done
  echo '{"type":"forge_test_argv","argv":'"$out]"'}'
}
