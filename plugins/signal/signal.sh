#!/bin/sh
# A Signal bridge: outbound follows `forge events` the way the reference
# plugin does (plugins/notify/notify.sh), keeping its cursor in
# `$FORGE_PLUGIN_STATE`; inbound polls `signal-cli receive` and turns a
# reply into a `forge` call. One process, two loops running side by
# side, so either one exiting ends the plugin and the manifest's
# `restart = "on-failure"` brings both back together.
set -u

: "${SIGNAL_CLI:=signal-cli}"

SIGNAL_ACCOUNT=
SIGNAL_TO=
SIGNAL_ALLOWED=
POLL_SECONDS=30
TARGET_REPO=
WORKFLOW=direct
NOTIFY_ON="blocked failed"
# A deploy that passes its check is quiet by default: a failed or
# rolled-back deploy always sends a message (see docs/DEPLOY.md, "When a
# deploy runs"). Set to 1 to also message on a deploy that simply passed.
NOTIFY_DEPLOY_OK=0

config="$FORGE_PLUGIN_DIR/config"
if [ -f "$config" ]; then
    while IFS= read -r cfgline || [ -n "$cfgline" ]; do
        case "$cfgline" in
            '' | '#'*) continue ;;
        esac
        key=${cfgline%%=*}
        val=${cfgline#*=}
        case "$key" in
            SIGNAL_ACCOUNT) SIGNAL_ACCOUNT=$val ;;
            SIGNAL_TO) SIGNAL_TO=$val ;;
            SIGNAL_ALLOWED) SIGNAL_ALLOWED=$val ;;
            POLL_SECONDS) POLL_SECONDS=$val ;;
            TARGET_REPO) TARGET_REPO=$val ;;
            WORKFLOW) WORKFLOW=$val ;;
            NOTIFY_ON) NOTIFY_ON=$val ;;
            NOTIFY_DEPLOY_OK) NOTIFY_DEPLOY_OK=$val ;;
        esac
    done <"$config"
fi

log() {
    printf 'signal: %s\n' "$1" >&2
}

# A JSON string field's value, escapes and all, from a compact
# single-line document (an events.jsonl line) on stdin.
json_str() {
    sed -n 's/.*"'"$1"'":"\(\([^"\\]\|\\.\)*\)".*/\1/p'
}

# Same, for the pretty-printed documents a `--json` verb prints (a space
# may follow the colon).
json_str_pretty() {
    sed -n 's/.*"'"$1"'": *"\(\([^"\\]\|\\.\)*\)".*/\1/p'
}

signal_send() {
    msg=$1
    case "$SIGNAL_TO" in
        group.*)
            "$SIGNAL_CLI" -a "$SIGNAL_ACCOUNT" send -m "$msg" -g "$SIGNAL_TO" >/dev/null 2>&1 \
                || log "send failed: $msg"
            ;;
        *)
            "$SIGNAL_CLI" -a "$SIGNAL_ACCOUNT" send -m "$msg" "$SIGNAL_TO" >/dev/null 2>&1 \
                || log "send failed: $msg"
            ;;
    esac
}

# The question `forge requests --json` records for one blocked task id.
# A RequestRow is flat (no nested objects), so the few lines right after
# its "id" line hold everything else that row has.
question_for() {
    "$FORGE_BIN" requests --json \
        | grep -A6 "\"id\": $1," \
        | json_str_pretty question \
        | head -n1
}

outbound() {
    cursor="$FORGE_PLUGIN_STATE/cursor"
    if [ -f "$cursor" ]; then
        offset=$(cat "$cursor")
    else
        # No cursor yet: start from the snapshot's offset, not from
        # zero, so a first run never replays history.
        offset=$("$FORGE_BIN" snapshot | sed -n 's/.*"events_offset": *\([0-9]*\).*/\1/p')
    fi

    "$FORGE_BIN" events --since "$offset" --follow | while IFS= read -r line; do
        offset=$((offset + $(printf '%s' "$line" | wc -c) + 1))
        printf '%s\n' "$offset" >"$cursor"

        type=$(printf '%s\n' "$line" | json_str type)

        if [ "$type" = deploy_finished ]; then
            ok=$(printf '%s\n' "$line" | sed -n 's/.*"ok":\(true\|false\).*/\1/p')
            if [ "$ok" = false ] || [ "$NOTIFY_DEPLOY_OK" = 1 ]; then
                project=$(printf '%s\n' "$line" | json_str project)
                target=$(printf '%s\n' "$line" | json_str target)
                sha=$(printf '%s\n' "$line" | json_str sha)
                rolled_back_to=$(printf '%s\n' "$line" | json_str rolled_back_to)
                if [ "$ok" = true ]; then
                    status=ok
                elif [ -n "$rolled_back_to" ]; then
                    status="rolled back to $rolled_back_to"
                else
                    status=failed
                fi
                signal_send "deploy $project/$target @ $sha: $status"
            fi
            continue
        fi

        [ "$type" = task_done ] || continue

        task=$(printf '%s\n' "$line" | sed -n 's/.*"task":\([0-9]*\).*/\1/p')
        state=$(printf '%s\n' "$line" | json_str state)

        case " $NOTIFY_ON " in
            *" $state "*) ;;
            *) continue ;;
        esac

        # The reason may embed a JSON-escaped newline (a literal
        # backslash-n); cut there for "the first line of its reason".
        reason=$(printf '%s\n' "$line" | json_str reason | sed 's/\\n.*//')
        msg="task $task $state: $reason"

        if [ "$state" = blocked ]; then
            q=$(question_for "$task")
            [ -n "$q" ] && msg=$(printf '%s\nquestion: %s' "$msg" "$q")
        fi

        signal_send "$msg"
    done
}

allowed() {
    sender=$1
    for a in $SIGNAL_ALLOWED; do
        [ "$a" = "$sender" ] && return 0
    done
    return 1
}

# $1: the message body from an allowed sender. Always passed to `forge`
# as one argv entry, never through a shell that could interpret it: the
# only commands this plugin recognizes are `/answer` and `/status`, and
# everything else is external data handed to `forge add` as the task's
# text, not instructions to this script.
handle_message() {
    body=$1
    case "$body" in
        /answer\ *)
            rest=${body#/answer }
            id=${rest%% *}
            text=${rest#* }
            [ "$text" = "$rest" ] && text=""
            if "$FORGE_BIN" answer "$id" "$text" >/dev/null 2>&1; then
                signal_send "answered task $id"
            else
                signal_send "could not answer task $id"
            fi
            ;;
        /status)
            snap=$("$FORGE_BIN" snapshot)
            queued=$(printf '%s\n' "$snap" | grep -c '"state": *"queued"')
            running=$(printf '%s\n' "$snap" | grep -c '"state": *"running"')
            blocked=$(printf '%s\n' "$snap" | grep -c '"state": *"blocked"')
            worker=$(printf '%s\n' "$snap" | sed -n 's/.*"running": *\(true\|false\).*/\1/p' | head -n1)
            signal_send "queued=$queued running=$running blocked=$blocked worker=$worker"
            ;;
        *)
            out=$("$FORGE_BIN" add "$TARGET_REPO" "$body" --workflow "$WORKFLOW" 2>&1)
            id=$(printf '%s\n' "$out" | sed -n 's/.*queued task \([0-9]*\).*/\1/p')
            if [ -n "$id" ]; then
                signal_send "queued task $id"
            else
                signal_send "could not queue: $out"
            fi
            ;;
    esac
}

inbound() {
    while :; do
        "$SIGNAL_CLI" -a "$SIGNAL_ACCOUNT" receive --json 2>/dev/null | while IFS= read -r line; do
            case "$line" in
                *'"dataMessage"'*) ;;
                *) continue ;;
            esac
            sender=$(printf '%s\n' "$line" | json_str sourceNumber)
            [ -n "$sender" ] || sender=$(printf '%s\n' "$line" | json_str source)
            body=$(printf '%s\n' "$line" | json_str message)
            if allowed "$sender"; then
                handle_message "$body"
            else
                log "ignoring message from ${sender:-an unknown sender}, not in SIGNAL_ALLOWED"
            fi
        done
        sleep "$POLL_SECONDS"
    done
}

outbound &
out_pid=$!
inbound &
in_pid=$!

cleanup() {
    kill "$out_pid" "$in_pid" 2>/dev/null
}
trap cleanup EXIT

# If either loop ever exits, so does this script, so the supervisor's
# restart policy brings both back rather than leaving one running alone.
while kill -0 "$out_pid" 2>/dev/null && kill -0 "$in_pid" 2>/dev/null; do
    sleep 1
done
log "a loop exited; stopping so the supervisor restarts both"
exit 1
