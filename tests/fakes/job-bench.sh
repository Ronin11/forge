#!/bin/bash
# `forge job bench`'s two providers, told apart by `JOB_BENCH_PROVIDER`
# (set through each provider's own `[providers.<name>].env`, since
# `agent_bin_for` keys on the step name alone and both providers run the
# same directive step): the hosted one classifies every fixture right;
# the local one is free and slower, and mislabels the fix as a chore.
prompt=$(cat)
kind=chore
line="misc"
case "$prompt" in
  *"release a blocked dependent"*)
    kind=fix
    line="tasks: a blocked dependent is released on any terminal state, not just a retry's reroute"
    ;;
  *"worker claims queued jobs"*)
    kind=feature
    line="jobs: the worker now claims and runs queued jobs alongside tasks"
    ;;
  *"steps before the tables"*)
    kind=docs
    line="docs: the run workflow example now parses (steps before [trigger])"
    ;;
  *"fmt and clippy fixes"*)
    kind=chore
    line="jobs: fmt and clippy fixes for the directive-step executor"
    ;;
esac

cost=0.0021
if [ "${JOB_BENCH_PROVIDER:-}" = "devhome" ]; then
  cost=0.0
  sleep 0.2
  if [ "$kind" = "fix" ]; then
    kind=chore
  fi
fi

printf '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":%s,"result":"ok","structured_output":{"line":"%s","kind":"%s"}}\n' \
  "$cost" "$line" "$kind"
