#!/bin/sh
# The reference plugin (docs/PLUGINS.md): a plugin is a process that
# speaks the CLI and imports nothing. Everything below is a `$FORGE_BIN`
# call and plain text processing on its output; no client library, no
# language runtime Forge had to build.
set -u

cursor="$FORGE_PLUGIN_STATE/cursor"
if [ -f "$cursor" ]; then
    offset=$(cat "$cursor")
else
    # No cursor yet: start from the snapshot's offset, not from zero, so a
    # first run never replays history.
    offset=$("$FORGE_BIN" snapshot | sed -n 's/.*"events_offset": *\([0-9]*\).*/\1/p')
fi

"$FORGE_BIN" events --since "$offset" --follow | while IFS= read -r line; do
    # `forge events` prints each events.jsonl line verbatim (minus its
    # newline), so the byte length read back plus one is exactly the
    # offset advance a fresh `--since` would need.
    offset=$((offset + $(printf '%s' "$line" | wc -c) + 1))
    type=$(printf '%s\n' "$line" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
    if [ "$type" = task_done ]; then
        task=$(printf '%s\n' "$line" | sed -n 's/.*"task":\([0-9]*\).*/\1/p')
        state=$(printf '%s\n' "$line" | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')
        # `events` prints each line's keys sorted, so "state" always
        # follows "reason"; the reason is JSON-escaped, so a real newline
        # is the two bytes `\n`, and cutting there keeps just its first line.
        reason=$(printf '%s\n' "$line" |
            sed -n 's/.*"reason":"\(.*\)","state":.*/\1/p' | sed 's/\\n.*//')
        printf '%s' "$line" |
            sh "$FORGE_PLUGIN_DIR/command" "$task" "$state" "$reason" || true
    fi
    printf '%s\n' "$offset" >"$cursor"
done
