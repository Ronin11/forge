#!/bin/sh
# The data half of Forge 1's status-file plugin (docs/PLUGINS.md): writes
# a status document a bar widget can read, atomically, on every event that
# changes the picture and on a five-second heartbeat otherwise. Everything
# in the document comes from `$FORGE_BIN snapshot` and
# `$FORGE_BIN requests --json`; this script only filters and counts what
# those already serve, it never computes a fact the CLI does not.
set -u

config="$FORGE_PLUGIN_DIR/config"
if [ -f "$config" ]; then
    . "$config"
fi

state_home="${XDG_STATE_HOME:-$HOME/.local/state}"
out_dir="$state_home/forge"
mkdir -p "$out_dir"
out_file="$out_dir/status.json"

# The one place state is derived, in this order: a question waiting on the
# operator or a task that failed within the last hour demands attention;
# otherwise a running task means work is in progress; otherwise idle.
compute_state() {
    questions=$1
    failed_recently=$2
    running=$3
    if [ "$questions" -gt 0 ] || [ "$failed_recently" = "true" ]; then
        printf attention
    elif [ "$running" -gt 0 ]; then
        printf working
    else
        printf idle
    fi
}

write_status() {
    now=$(date +%s)
    snap=$("$FORGE_BIN" snapshot) || return 0
    reqs=$("$FORGE_BIN" requests --json) || reqs='[]'
    questions=$(printf '%s' "$reqs" | jq 'length')
    running=$(printf '%s' "$snap" | jq '[.tasks[] | select(.state == "running")] | length')
    # A task counts as recently failed by when it finished, not when it was
    # created: a task can sit queued or run long enough that its creation
    # time is over an hour old even though it failed seconds ago.
    failed_recently=$(printf '%s' "$snap" | jq --argjson now "$now" \
        '([ .tasks[] | select(.state == "failed" and .finished_at != null and (($now - .finished_at) < 3600)) ] | length > 0)')
    state=$(compute_state "$questions" "$failed_recently" "$running")

    doc=$(printf '%s' "$snap" | jq \
        --argjson now "$now" \
        --arg state "$state" \
        --argjson questions "$questions" \
        --argjson failed_recently "$failed_recently" \
        --arg ui "${FORGE_WEB_URL:-}" \
        '{
            schema: 1,
            ts: $now,
            state: $state,
            running: [ .tasks[] | select(.state == "running") | {
                id: .id,
                repo: (.repo | split("/") | last),
                workflow: .workflow,
                elapsed_s: ($now - .created_at)
            } ],
            queued: ([ .tasks[] | select(.state == "queued") ] | length),
            blocked: ([ .tasks[] | select(.state == "blocked") ] | length),
            failed_recently: $failed_recently,
            questions: $questions
        } + (if $ui == "" then {} else {ui: $ui} end)')

    # Atomic: write into a temp file in the same directory, then rename, so
    # a reader never sees a partial document.
    tmp="$out_dir/.status.json.tmp.$$"
    printf '%s\n' "$doc" >"$tmp"
    mv -f "$tmp" "$out_file"
}

cursor="$FORGE_PLUGIN_STATE/cursor"
if [ -f "$cursor" ]; then
    offset=$(cat "$cursor")
else
    # No cursor yet: start from the snapshot's offset, not from zero, so a
    # first run never replays history.
    offset=$("$FORGE_BIN" snapshot | jq -r '.events_offset // 0')
fi

write_status

# A FIFO, not a shell pipeline: `$!` after `a | b &` names only the last
# stage, so the follower (`forge events --follow`) would otherwise have no
# PID we can reach to stop it, and it would sit blocked on its next read
# forever after we stop reading. With a FIFO both ends have their own PID.
fifo="$FORGE_PLUGIN_STATE/events.fifo"
rm -f "$fifo"
mkfifo "$fifo"

"$FORGE_BIN" events --since "$offset" --follow >"$fifo" &
events_pid=$!

(
    while IFS= read -r line; do
        # `forge events` prints each events.jsonl line verbatim (minus its
        # newline), so the byte length read back plus one is exactly the
        # offset advance a fresh `--since` would need.
        offset=$((offset + $(printf '%s' "$line" | wc -c) + 1))
        printf '%s\n' "$offset" >"$cursor"
        write_status
    done <"$fifo"
) &
reader_pid=$!

cleanup() {
    kill "$events_pid" "$reader_pid" 2>/dev/null || true
    rm -f "$fifo"
}
trap cleanup EXIT
trap 'cleanup; exit 0' INT TERM

# The heartbeat: even with no events, rewrite the document every five
# seconds so a consumer can treat a stale `ts` as Forge being down.
while true; do
    sleep 5
    write_status
done
