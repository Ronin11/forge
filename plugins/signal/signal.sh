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
# Intake's addressee mechanism (docs/INTAKE.md): a blocked question can
# name who it is for (`needs_input.to`, carried as `to` on `forge
# requests --json` and `question_to` on the task). When that name
# matches an entry here, the question goes to their number instead of
# the operator's, and a reply from that number while their question is
# open is submitted as their answer, not queued as a new task.
# Space-separated "name:number" pairs.
CONTACTS=
# Which project a CONTACTS name's plain message is filed against, as
# "name:project" pairs (space-separated). A name PROJECTS does not list
# falls to TARGET_REPO's own project (see `target_repo_project`), the
# same repo->project link `forge add` resolves without a --project.
PROJECTS=
POLL_SECONDS=30
TARGET_REPO=
WORKFLOW=reviewed
NOTIFY_ON="blocked failed"
# A deploy that passes its check is quiet by default: a failed or
# rolled-back deploy always sends a message (see docs/DEPLOY.md, "When a
# deploy runs"). Set to 1 to also message on a deploy that simply passed.
NOTIFY_DEPLOY_OK=0
# Where the operator's reverse proxy serves the customer portal (see
# docs/PORTAL.md and docs/DEPLOY.md, "Provisioning"), e.g.
# https://portal.example.com. A portal link is sent as
# "$PORTAL_URL/p/<token>"; left empty, the bare "/p/<token>" path is sent.
PORTAL_URL=

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
            CONTACTS) CONTACTS=$val ;;
            PROJECTS) PROJECTS=$val ;;
            POLL_SECONDS) POLL_SECONDS=$val ;;
            TARGET_REPO) TARGET_REPO=$val ;;
            WORKFLOW) WORKFLOW=$val ;;
            NOTIFY_ON) NOTIFY_ON=$val ;;
            NOTIFY_DEPLOY_OK) NOTIFY_DEPLOY_OK=$val ;;
            PORTAL_URL) PORTAL_URL=$val ;;
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

# The Signal number for a CONTACTS name, or nothing (and a non-zero
# exit) if it names no contact.
contact_number() {
    name=$1
    for pair in $CONTACTS; do
        n=${pair%%:*}
        num=${pair#*:}
        if [ "$n" = "$name" ]; then
            printf '%s\n' "$num"
            return 0
        fi
    done
    return 1
}

# The CONTACTS name for a Signal number, or nothing (and a non-zero
# exit) if it names no contact.
contact_name() {
    number=$1
    for pair in $CONTACTS; do
        n=${pair%%:*}
        num=${pair#*:}
        if [ "$num" = "$number" ]; then
            printf '%s\n' "$n"
            return 0
        fi
    done
    return 1
}

# The project PROJECTS names for CONTACTS name $1, or nothing (and a
# non-zero exit) if PROJECTS names no project for them.
project_for_contact() {
    name=$1
    for pair in $PROJECTS; do
        n=${pair%%:*}
        proj=${pair#*:}
        if [ "$n" = "$name" ]; then
            printf '%s\n' "$proj"
            return 0
        fi
    done
    return 1
}

# TARGET_REPO's own project: whichever project's repository list names
# it, read off the plain-text `forge project list` ("name" and "repo"
# lines) rather than parsed JSON, since this is one exact string match,
# not a document to walk. The same repo->project link `forge add`
# resolves on its own when a task names no --project.
target_repo_project() {
    "$FORGE_BIN" project list 2>/dev/null | awk -v repo="$TARGET_REPO" '
        /^name /  { name = $2 }
        /^repo /  { if ($2 == repo) { print name; exit } }
    '
}

# The project CONTACTS name $1's message should be filed against:
# PROJECTS names it directly, or TARGET_REPO's own project when it does
# not.
concierge_project() {
    name=$1
    if proj=$(project_for_contact "$name") && [ -n "$proj" ]; then
        printf '%s\n' "$proj"
        return 0
    fi
    target_repo_project
}

# Records one message in Forge's message record (docs/PLUGINS.md, "the
# message record" / `forge message`), so `forge message list` answers
# "has this contact replied since" for this channel. $1 project, $2
# direction ("in" or "out"), $3 contact, $4 text, $5 task id (optional).
# Best-effort and always quiet: a project this call can't name (empty)
# is skipped rather than failed, since a plugin's own bookkeeping must
# never be why a message wasn't sent or a reply wasn't filed. Uses
# `mr_`-prefixed variable names throughout, since this is called as a
# plain function (not `$(...)`) from callers that still need their own
# same-named variables (`project`, `contact`, `name`, `text`, `task`)
# afterward, and plain `sh` has no `local`.
record_message() {
    mr_project=$1
    mr_direction=$2
    mr_contact=$3
    mr_text=$4
    mr_task=${5:-}
    [ -n "$mr_project" ] || return 0
    case "$mr_direction" in
        in) mr_flag=--from ;;
        *) mr_flag=--to ;;
    esac
    if [ -n "$mr_task" ]; then
        "$FORGE_BIN" message record "$mr_project" --channel signal "$mr_flag" "$mr_contact" \
            --text "$mr_text" --task "$mr_task" >/dev/null 2>&1 \
            || log "could not record message for $mr_contact"
    else
        "$FORGE_BIN" message record "$mr_project" --channel signal "$mr_flag" "$mr_contact" \
            --text "$mr_text" >/dev/null 2>&1 \
            || log "could not record message for $mr_contact"
    fi
}

# Runs the concierge (docs/INTAKE.md, "The front door is not the
# interview") on CONTACTS name $1's message ($2), against project $3,
# and sends whichever reply it decided back to their Signal number ($4):
# the answer to a question, "on it" for a filed task (a request or a
# need — either way a task now exists to act on), or the question when
# the decision is unclear — the same open-question machinery
# `task_for_contact` picks their next reply up with, so the exchange
# continues as an interview would. Every message on this path is
# recorded, in both directions (see `record_message`).
concierge_reply() {
    name=$1
    body=$2
    project=$3
    dest=$4
    record_message "$project" in "$name" "$body"
    errs=$(mktemp)
    if out=$("$FORGE_BIN" ask "$project" "$body" --from "$name" 2>"$errs"); then
        first=$(printf '%s\n' "$out" | head -n1)
        case "$first" in
            "concierge: a request;"* | "concierge: a need"*)
                reply="on it"
                ;;
            "concierge: unclear;"*)
                reply=$(printf '%s\n' "$first" | sed 's/^.*with a question\( for [^:]*\)\{0,1\}: //')
                ;;
            *)
                reply=$first
                ;;
        esac
    else
        log "ask failed for $name: $(cat "$errs")"
        reply="could not process that; try again?"
    fi
    rm -f "$errs"
    signal_send "$dest" "$reply"
    record_message "$project" out "$name" "$reply"
}

signal_send() {
    dest=$1
    msg=$2
    case "$dest" in
        group.*)
            "$SIGNAL_CLI" -a "$SIGNAL_ACCOUNT" send -m "$msg" -g "$dest" >/dev/null 2>&1 \
                || log "send failed: $msg"
            ;;
        *)
            "$SIGNAL_CLI" -a "$SIGNAL_ACCOUNT" send -m "$msg" "$dest" >/dev/null 2>&1 \
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

# Same shape, for the addressee (`to`); empty when the question is for
# the operator.
question_to_for() {
    "$FORGE_BIN" requests --json \
        | grep -A6 "\"id\": $1," \
        | json_str_pretty to \
        | head -n1
}

# The blocked task id whose question is addressed to CONTACTS name $1,
# if any: scans every RequestRow for a "to" line matching it, and
# reports the "id" line that precedes it (a row is flat and prints id
# first), rather than assuming the -A6 window of `question_for` fits
# every row of a multi-row document.
task_for_contact() {
    name=$1
    "$FORGE_BIN" requests --json | awk -v want="$name" '
        /"id":/ { match($0, /[0-9]+/); id = substr($0, RSTART, RLENGTH) }
        /"to":/ {
            line = $0
            sub(/.*"to": *"/, "", line)
            sub(/".*/, "", line)
            if (line == want) { print id; exit }
        }
    '
}

# The project CONTACTS name $1's customer portal link should open:
# whatever `record_portal_project` last recorded for them (from a
# `project_created` event, see docs/PORTAL.md), or their own name if
# nothing has been recorded yet — `forge intake accept`'s default project
# name is the interviewed person's name, slugged, so for a plain name
# (the common case) this already matches before the event ever arrives.
contact_project() {
    name=$1
    map="$FORGE_PLUGIN_STATE/portal-projects"
    if [ -f "$map" ]; then
        proj=$(awk -v want="$name" '$1==want{p=$2} END{if(p)print p}' "$map")
        if [ -n "$proj" ]; then
            printf '%s\n' "$proj"
            return 0
        fi
    fi
    printf '%s\n' "$name"
}

# Remembers that CONTACTS name $1's portal project is $2, so a later
# `/portal` request (or another project of theirs, later) resolves to it
# even if it doesn't match their own name.
record_portal_project() {
    printf '%s %s\n' "$1" "$2" >>"$FORGE_PLUGIN_STATE/portal-projects"
}

# Mints a fresh customer portal link for CONTACTS name $1's project (see
# docs/PORTAL.md) and sends it to Signal destination $2. Best-effort: a
# person with no project yet (or a name `forge project portal` doesn't
# recognize) gets nothing rather than an error message about internals.
send_portal_link() {
    name=$1
    dest=$2
    project=$(contact_project "$name")
    link=$("$FORGE_BIN" project portal "$project" 2>/dev/null | tail -n1)
    case "$link" in
        /p/*)
            msg="Your Forge portal: ${PORTAL_URL}${link}"
            signal_send "$dest" "$msg"
            record_message "$project" out "$name" "$msg"
            ;;
        *) log "could not mint a portal link for $name (project $project)" ;;
    esac
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
                msg="deploy $project/$target @ $sha: $status"
                signal_send "$SIGNAL_TO" "$msg"
                record_message "$project" out operator "$msg"
            fi
            continue
        fi

        if [ "$type" = project_created ]; then
            project=$(printf '%s\n' "$line" | json_str project)
            person=$(printf '%s\n' "$line" | json_str person)
            num=$(contact_number "$person")
            if [ -n "$num" ]; then
                record_portal_project "$person" "$project"
                send_portal_link "$person" "$num"
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

        # A blocked question addressed to a configured contact goes to
        # them, not the operator's SIGNAL_TO; unaddressed (or addressed
        # to a name CONTACTS does not know) still reaches the operator.
        dest="$SIGNAL_TO"
        notify_contact=operator
        if [ "$state" = blocked ]; then
            q=$(question_for "$task")
            [ -n "$q" ] && msg=$(printf '%s\nquestion: %s' "$msg" "$q")
            to_name=$(question_to_for "$task")
            if [ -n "$to_name" ]; then
                num=$(contact_number "$to_name") && [ -n "$num" ] && dest="$num"
                notify_contact="$to_name"
            fi
        fi

        signal_send "$dest" "$msg"
        record_message "$(target_repo_project)" out "$notify_contact" "$msg" "$task"
    done
}

allowed() {
    sender=$1
    for a in $SIGNAL_ALLOWED; do
        [ "$a" = "$sender" ] && return 0
    done
    return 1
}

# $1: the message body from an allowed sender or a CONTACTS name, $2:
# where the reply goes (an allowed sender's own SIGNAL_TO, or a
# contact's own number). Always passed to `forge` as one argv entry,
# never through a shell that could interpret it: the only commands this
# plugin recognizes are `/answer`, `/status` and `/help`, and everything
# else is external data handed to `forge add` as the task's text, not
# instructions to this script.
handle_message() {
    body=$1
    dest=$2
    project=$(target_repo_project)
    case "$body" in
        /answer\ *)
            rest=${body#/answer }
            id=${rest%% *}
            text=${rest#* }
            [ "$text" = "$rest" ] && text=""
            if "$FORGE_BIN" answer "$id" "$text" >/dev/null 2>&1; then
                msg="answered task $id"
            else
                msg="could not answer task $id"
            fi
            signal_send "$dest" "$msg"
            record_message "$project" out "$dest" "$msg" "$id"
            ;;
        /status)
            snap=$("$FORGE_BIN" snapshot)
            queued=$(printf '%s\n' "$snap" | grep -c '"state": *"queued"')
            running=$(printf '%s\n' "$snap" | grep -c '"state": *"running"')
            blocked=$(printf '%s\n' "$snap" | grep -c '"state": *"blocked"')
            worker=$(printf '%s\n' "$snap" | sed -n 's/.*"running": *\(true\|false\).*/\1/p' | head -n1)
            msg="queued=$queued running=$running blocked=$blocked worker=$worker"
            signal_send "$dest" "$msg"
            record_message "$project" out "$dest" "$msg"
            ;;
        /help)
            msg="commands: /answer <id> <text>, /status; anything else is filed as a new task"
            signal_send "$dest" "$msg"
            record_message "$project" out "$dest" "$msg"
            ;;
        *)
            out=$("$FORGE_BIN" add "$TARGET_REPO" "$body" --workflow "$WORKFLOW" --trust contact 2>&1)
            id=$(printf '%s\n' "$out" | sed -n 's/.*queued task \([0-9]*\).*/\1/p')
            if [ -n "$id" ]; then
                msg="queued task $id"
            else
                msg="could not queue: $out"
            fi
            signal_send "$dest" "$msg"
            record_message "$project" out "$dest" "$msg" "$id"
            ;;
    esac
}

# A reply from a CONTACTS name ($1) at their own number ($2), while a
# question addressed to them is open ($3, the task id `task_for_contact`
# found): submitted as their answer, the same `/answer` path an allowed
# sender drives by hand, except the contact never names the task
# themselves — the one open question addressed to them is the only one
# it can be.
handle_contact_reply() {
    name=$1
    number=$2
    id=$3
    body=$4
    project=$(contact_project "$name")
    record_message "$project" in "$name" "$body" "$id"
    if "$FORGE_BIN" answer "$id" "$body" --by "$name" >/dev/null 2>&1; then
        msg="answered task $id"
    else
        msg="could not answer task $id"
    fi
    signal_send "$number" "$msg"
    record_message "$project" out "$name" "$msg" "$id"
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
            name=$(contact_name "$sender")
            id=""
            [ -n "$name" ] && id=$(task_for_contact "$name")
            if [ -n "$name" ] && [ "$body" = "/portal" ]; then
                send_portal_link "$name" "$sender"
            elif [ -n "$name" ] && [ -n "$id" ]; then
                handle_contact_reply "$name" "$sender" "$id" "$body"
            elif [ -n "$name" ]; then
                case "$body" in
                    /answer\ * | /status | /help)
                        handle_message "$body" "$sender"
                        ;;
                    *)
                        project=$(concierge_project "$name")
                        if [ -n "$project" ]; then
                            concierge_reply "$name" "$body" "$project" "$sender"
                        else
                            log "no project for $name (PROJECTS names none and TARGET_REPO's is not on record); ignoring"
                        fi
                        ;;
                esac
            elif allowed "$sender"; then
                handle_message "$body" "$SIGNAL_TO"
            else
                log "ignoring message from ${sender:-an unknown sender}, not in SIGNAL_ALLOWED or CONTACTS"
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
