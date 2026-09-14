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
