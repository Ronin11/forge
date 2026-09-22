#!/bin/sh
# Search, buy and release Forge's own Twilio phone numbers
# (docs/PLUGINS.md). `search` and `owned` are read-only and always
# allowed; `buy` is gated the way `forge provision` gates standing up a
# Hetzner box (docs/DEPLOY.md, "Provisioning"): it requires an explicit
# `--yes` on the command line, run by hand, and it is never called from
# this script's own supervised loop (the no-argument case below) or from
# any workflow step an agent could reach. `release` carries the same
# `--yes` gate, symmetrically.
set -u

: "${CURL:=curl}"
: "${TWILIO_API:=https://api.twilio.com/2010-04-01}"
: "${TWILIO_PRICING_API:=https://pricing.twilio.com/v1}"

TWILIO_ACCOUNT_SID=
TWILIO_AUTH_TOKEN=
TWILIO_API_KEY_SID=
TWILIO_API_KEY_SECRET=
MONTHLY_CAP_USD=5
MAX_NUMBERS=2
COUNTRY=US
POLL_SECS=30
ALLOWED=
CONTACTS=
PROJECTS=
TARGET_REPO=
WORKFLOW=direct

config="$FORGE_PLUGIN_DIR/config"
if [ -f "$config" ]; then
    # Refuse a config readable or writable by group or other, regardless
    # of what verb was asked for: it holds an account SID and a token
    # (or API key secret) that can buy numbers on Forge's account.
    mode=$(stat -c %a "$config" 2>/dev/null)
    if [ -z "$mode" ]; then
        mode=$(stat -f %Lp "$config" 2>/dev/null)
    fi
    if [ -n "$mode" ]; then
        last2=$(printf '%s' "$mode" | sed 's/.*\(..\)$/\1/')
        group=$(printf '%s' "$last2" | cut -c1)
        other=$(printf '%s' "$last2" | cut -c2)
        if [ "$group" != "0" ] || [ "$other" != "0" ]; then
            echo "twilio: refusing to run: $config is readable by group or other (mode $mode); chmod 600 it" >&2
            exit 1
        fi
    fi
    while IFS= read -r cfgline || [ -n "$cfgline" ]; do
        case "$cfgline" in
            '' | '#'*) continue ;;
        esac
        key=${cfgline%%=*}
        val=${cfgline#*=}
        case "$key" in
            TWILIO_ACCOUNT_SID) TWILIO_ACCOUNT_SID=$val ;;
            TWILIO_AUTH_TOKEN) TWILIO_AUTH_TOKEN=$val ;;
            TWILIO_API_KEY_SID) TWILIO_API_KEY_SID=$val ;;
            TWILIO_API_KEY_SECRET) TWILIO_API_KEY_SECRET=$val ;;
            MONTHLY_CAP_USD) MONTHLY_CAP_USD=$val ;;
            MAX_NUMBERS) MAX_NUMBERS=$val ;;
            COUNTRY) COUNTRY=$val ;;
            POLL_SECS) POLL_SECS=$val ;;
            ALLOWED) ALLOWED=$val ;;
            CONTACTS) CONTACTS=$val ;;
            PROJECTS) PROJECTS=$val ;;
            TARGET_REPO) TARGET_REPO=$val ;;
            WORKFLOW) WORKFLOW=$val ;;
        esac
    done <"$config"
fi

# An API key pair, when set, authenticates in place of the account's own
# auth token (see config.example).
auth_user=$TWILIO_ACCOUNT_SID
auth_pass=$TWILIO_AUTH_TOKEN
if [ -n "$TWILIO_API_KEY_SID" ]; then
    auth_user=$TWILIO_API_KEY_SID
    auth_pass=$TWILIO_API_KEY_SECRET
fi

numbers_file="$FORGE_PLUGIN_STATE/numbers.json"
[ -f "$numbers_file" ] || printf '[]' >"$numbers_file"

log() {
    printf 'twilio: %s\n' "$1" >&2
}

# Percent-encodes $1 for use in a query string or a URL path segment
# (an E.164 number's leading '+' is the one character this ever needs
# to escape).
urlenc() {
    jq -rn --arg v "$1" '$v | @uri'
}

api_get() {
    "$CURL" -s -u "$auth_user:$auth_pass" "$TWILIO_API/Accounts/$TWILIO_ACCOUNT_SID/$1"
}

# The Pricing API's current monthly price for a local number in COUNTRY
# (Twilio prices by number type and country, not per individual number,
# so this one lookup covers every number `search` or `owned` shows for
# that country). Empty if the Pricing API has nothing for it.
country_local_price() {
    doc=$("$CURL" -s -u "$auth_user:$auth_pass" "$TWILIO_PRICING_API/PhoneNumbers/Countries/$COUNTRY.json")
    printf '%s' "$doc" | jq -r '.phone_number_prices[]? | select(.number_type=="local") | .current_price' | head -n1
}

# The price to show for an owned number: whatever `buy` recorded for it
# in numbers.json, or (a number Forge holds but this tool didn't buy)
# the country's current local price as a fallback.
price_for_number() {
    num=$1
    p=$(jq -r --arg n "$num" '[.[] | select(.number==$n)][0].price // empty' "$numbers_file")
    if [ -z "$p" ]; then
        p=$(country_local_price)
    fi
    printf '%s\n' "$p"
}

owned_json() {
    api_get "IncomingPhoneNumbers.json?PageSize=1000"
}

# "<count> <total monthly cost>" across every number Twilio's own
# IncomingPhoneNumbers list reports Forge holding right now: the source
# of truth for `buy`'s caps is the account, not this tool's own record.
owned_totals() {
    doc=$(owned_json)
    count=$(printf '%s' "$doc" | jq '.incoming_phone_numbers | length')
    cost=0
    numbers=$(printf '%s' "$doc" | jq -r '.incoming_phone_numbers[].phone_number')
    for n in $numbers; do
        p=$(price_for_number "$n")
        [ -z "$p" ] && p=0
        cost=$(awk -v c="$cost" -v p="$p" 'BEGIN{printf "%.4f", c+p}')
    done
    printf '%s %s\n' "$count" "$cost"
}

# Whether AvailablePhoneNumbers would currently return $1 as available:
# what `buy` must check before ever purchasing a number, so buying can
# never reach for one search would not have offered.
number_offered() {
    number=$1
    doc=$(api_get "AvailablePhoneNumbers/$COUNTRY/Local.json?Contains=$(urlenc "$number")")
    printf '%s' "$doc" | jq -e --arg n "$number" \
        '[.available_phone_numbers[]? | select(.phone_number==$n)] | length > 0' >/dev/null
}

record_bought() {
    number=$1
    sid=$2
    price=$3
    bought_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    tmp=$(mktemp)
    jq --arg number "$number" --arg sid "$sid" --arg price "$price" --arg bought_at "$bought_at" \
        '. + [{number: $number, sid: $sid, bought_at: $bought_at, price: $price}]' \
        "$numbers_file" >"$tmp" && mv "$tmp" "$numbers_file"
}

record_released() {
    number=$1
    tmp=$(mktemp)
    jq --arg number "$number" '[.[] | select(.number != $number)]' "$numbers_file" >"$tmp" && mv "$tmp" "$numbers_file"
}

# GET AvailablePhoneNumbers/{COUNTRY}/Local.json: read-only, always
# allowed. Prints one line per number: the number itself, its locality,
# the capabilities Twilio reports (voice, SMS, MMS, fax), and the
# monthly price from the Pricing API (when it has one for this country).
search() {
    area_code=""
    sms_only=0
    voice_only=0
    limit=20
    while [ $# -gt 0 ]; do
        case "$1" in
            --area-code)
                area_code=$2
                shift 2
                ;;
            --sms)
                sms_only=1
                shift
                ;;
            --voice)
                voice_only=1
                shift
                ;;
            --limit)
                limit=$2
                shift 2
                ;;
            *)
                log "search: unknown argument $1"
                exit 1
                ;;
        esac
    done

    query="PageSize=$limit"
    [ -n "$area_code" ] && query="$query&AreaCode=$area_code"
    [ "$sms_only" = 1 ] && query="$query&SmsEnabled=true"
    [ "$voice_only" = 1 ] && query="$query&VoiceEnabled=true"

    doc=$(api_get "AvailablePhoneNumbers/$COUNTRY/Local.json?$query")
    price=$(country_local_price)
    [ -z "$price" ] && price="?"

    printf '%s\n' "$doc" | jq -r --arg price "$price" '
        .available_phone_numbers[] |
        [
            .phone_number,
            (.locality // "?"),
            ([.capabilities | to_entries[] | select(.value) | .key] | join(",")),
            $price
        ] | @tsv
    ' | while IFS="$(printf '\t')" read -r number locality caps p; do
        printf '%s  locality=%s  capabilities=%s  price=%s/mo\n' "$number" "$locality" "$caps" "$p"
    done
}

# GET IncomingPhoneNumbers.json: every number Forge's Twilio account
# currently holds, with its monthly price (see `price_for_number`).
owned() {
    doc=$(owned_json)
    printf '%s\n' "$doc" | jq -r '.incoming_phone_numbers[] | [.phone_number, .sid] | @tsv' \
        | while IFS="$(printf '\t')" read -r number sid; do
            price=$(price_for_number "$number")
            [ -z "$price" ] && price="?"
            printf '%s  sid=%s  price=%s/mo\n' "$number" "$sid" "$price"
        done
}

# `<number> --yes`: buys a Twilio number for Forge's own use. Refuses
# without --yes, refuses a number `search` would not currently offer,
# and refuses a purchase that would push Forge past MAX_NUMBERS or
# MONTHLY_CAP_USD (this account's current owned total, from Twilio
# itself, plus this number's price). On success, records the purchase
# in $FORGE_PLUGIN_STATE/numbers.json and prints the number and its
# Twilio SID as the last line.
buy() {
    number=""
    yes=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --yes)
                yes=1
                shift
                ;;
            -*)
                log "buy: unknown argument $1"
                exit 1
                ;;
            *)
                number=$1
                shift
                ;;
        esac
    done

    if [ -z "$number" ]; then
        log "buy: an E.164 number is required"
        exit 1
    fi
    if [ "$yes" -ne 1 ]; then
        log "buy: refusing to buy $number without --yes"
        exit 1
    fi

    if ! number_offered "$number"; then
        log "buy: $number is not offered by search; refusing"
        exit 1
    fi

    price=$(country_local_price)
    [ -z "$price" ] && price=0

    set -- $(owned_totals)
    owned_count=$1
    owned_cost=$2

    if [ "$owned_count" -ge "$MAX_NUMBERS" ]; then
        log "buy: refusing, already holding $owned_count number(s) (MAX_NUMBERS=$MAX_NUMBERS)"
        exit 1
    fi

    projected=$(awk -v c="$owned_cost" -v p="$price" 'BEGIN{printf "%.4f", c+p}')
    if awk -v proj="$projected" -v cap="$MONTHLY_CAP_USD" 'BEGIN{exit !(proj>cap)}'; then
        log "buy: refusing, \$$projected/mo would exceed MONTHLY_CAP_USD=\$$MONTHLY_CAP_USD"
        exit 1
    fi

    resp=$("$CURL" -s -u "$auth_user:$auth_pass" -X POST \
        "$TWILIO_API/Accounts/$TWILIO_ACCOUNT_SID/IncomingPhoneNumbers.json" \
        --data-urlencode "PhoneNumber=$number")
    sid=$(printf '%s' "$resp" | jq -r '.sid // empty')
    if [ -z "$sid" ]; then
        log "buy: Twilio did not return a sid: $resp"
        exit 1
    fi

    record_bought "$number" "$sid" "$price"
    log "bought $number, now \$${projected}/mo"
    printf '%s %s\n' "$number" "$sid"
}

# `<number> --yes`: releases a Twilio number Forge holds. Refuses
# without --yes. Looks the number's SID up in numbers.json first (the
# common case, a number this tool bought); falls back to asking Twilio
# for a number it holds but this tool didn't record.
release() {
    number=""
    yes=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --yes)
                yes=1
                shift
                ;;
            -*)
                log "release: unknown argument $1"
                exit 1
                ;;
            *)
                number=$1
                shift
                ;;
        esac
    done

    if [ -z "$number" ]; then
        log "release: an E.164 number is required"
        exit 1
    fi
    if [ "$yes" -ne 1 ]; then
        log "release: refusing to release $number without --yes"
        exit 1
    fi

    sid=$(jq -r --arg n "$number" '[.[] | select(.number==$n)][0].sid // empty' "$numbers_file")
    if [ -z "$sid" ]; then
        doc=$(api_get "IncomingPhoneNumbers.json?PhoneNumber=$(urlenc "$number")")
        sid=$(printf '%s' "$doc" | jq -r '.incoming_phone_numbers[0].sid // empty')
    fi
    if [ -z "$sid" ]; then
        log "release: $number is not a number Forge holds"
        exit 1
    fi

    code=$("$CURL" -s -o /dev/null -w '%{http_code}' -u "$auth_user:$auth_pass" -X DELETE \
        "$TWILIO_API/Accounts/$TWILIO_ACCOUNT_SID/IncomingPhoneNumbers/$sid.json")
    if [ "$code" != "204" ]; then
        log "release: DELETE failed ($code)"
        exit 1
    fi

    record_released "$number"
    log "released $number"
}

# The E.164 number for a CONTACTS name, or nothing (and a non-zero
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

# The CONTACTS name for an E.164 number, or nothing (and a non-zero
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

# TARGET_REPO's own project, read off the plain-text `forge project
# list` the same way the signal plugin does.
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

# Whether E.164 number $1 is in ALLOWED.
allowed() {
    sender=$1
    for a in $ALLOWED; do
        [ "$a" = "$sender" ] && return 0
    done
    return 1
}

# Records one message in Forge's message record (docs/PLUGINS.md, "the
# message record"), always by the contact's raw E.164 number (never a
# CONTACTS name), so `forge message list` answers "has this contact
# replied since" for this channel the same way for a known contact and
# a stranger alike. Best-effort and always quiet, the same posture as
# the signal plugin's own `record_message`: a project this call can't
# name (empty) is skipped rather than failed.
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
        "$FORGE_BIN" message record "$mr_project" --channel sms "$mr_flag" "$mr_contact" \
            --text "$mr_text" --task "$mr_task" >/dev/null 2>&1 \
            || log "could not record message for $mr_contact"
    else
        "$FORGE_BIN" message record "$mr_project" --channel sms "$mr_flag" "$mr_contact" \
            --text "$mr_text" >/dev/null 2>&1 \
            || log "could not record message for $mr_contact"
    fi
}

# Sends $3 from owned number $1 to $2 over Twilio's REST API (the same
# send path `send-sms.toml` uses for a job step's `message` effect).
# Best-effort: a failure is logged, never fatal, since one bad reply
# must not stop the poll loop.
twilio_reply() {
    from=$1
    to=$2
    text=$3
    resp=$("$CURL" -s -u "$auth_user:$auth_pass" -X POST \
        "$TWILIO_API/Accounts/$TWILIO_ACCOUNT_SID/Messages.json" \
        --data-urlencode "From=$from" \
        --data-urlencode "To=$to" \
        --data-urlencode "Body=$text")
    sid=$(printf '%s' "$resp" | jq -r '.sid // empty')
    [ -n "$sid" ] || log "send failed to $to: $resp"
}

# An allowed sender's message ($1) at $2, replying from owned number
# $3: `/answer <id> <text>` submits that answer, anything else queues
# new work via `forge add TARGET_REPO`, mirroring the signal plugin's
# own `handle_message`.
handle_message() {
    body=$1
    from=$2
    owned=$3
    project=$(target_repo_project)
    id=""
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
            ;;
        *)
            out=$("$FORGE_BIN" add "$TARGET_REPO" "$body" --workflow "$WORKFLOW" 2>&1)
            id=$(printf '%s\n' "$out" | sed -n 's/.*queued task \([0-9]*\).*/\1/p')
            if [ -n "$id" ]; then
                msg="queued task $id"
            else
                msg="could not queue: $out"
            fi
            ;;
    esac
    twilio_reply "$owned" "$from" "$msg"
    record_message "$project" out "$from" "$msg" "$id"
}

# A reply from CONTACTS name $1 at number $2, replying from owned
# number $3, while a question addressed to them is open ($4, the task
# id `task_for_contact` found): submitted as their answer, the same
# `/answer` path an allowed sender drives by hand, except the contact
# never names the task themselves.
handle_contact_reply() {
    name=$1
    from=$2
    owned=$3
    id=$4
    body=$5
    project=$(concierge_project "$name")
    if "$FORGE_BIN" answer "$id" "$body" --by "$name" >/dev/null 2>&1; then
        msg="answered task $id"
    else
        msg="could not answer task $id"
    fi
    twilio_reply "$owned" "$from" "$msg"
    record_message "$project" out "$from" "$msg" "$id"
}

# Runs the concierge (docs/INTAKE.md, "The front door is not the
# interview") on CONTACTS name $1's message ($2), against project $3,
# replying from owned number $5 back to their number $4: the answer to
# a question, "on it" for a filed task, or the question when the
# decision is unclear.
concierge_reply() {
    name=$1
    body=$2
    project=$3
    from=$4
    owned=$5
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
    twilio_reply "$owned" "$from" "$reply"
    record_message "$project" out "$from" "$reply"
}

# The question `forge requests --json` records for one blocked task id,
# and the blocked task id whose question is addressed to CONTACTS name
# $1, the same way the signal plugin finds them.
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

# One newly-seen inbound message: recorded in the message record (by
# its raw E.164 sender, docs/PLUGINS.md), then routed the way the
# signal plugin routes an inbound message, reusing its config names
# where they apply — a CONTACTS name with an open question answers it;
# a CONTACTS name otherwise routes through the concierge (PROJECTS);
# an ALLOWED sender queues work or answers by `/answer`; anyone else is
# recorded and logged, nothing queued (docs/PLUGINS.md, "Trust").
handle_inbound() {
    owned=$1
    from=$2
    body=$3
    name=$(contact_name "$from")
    if [ -n "$name" ]; then
        project=$(concierge_project "$name")
    else
        project=$(target_repo_project)
    fi
    record_message "$project" in "$from" "$body"

    id=""
    [ -n "$name" ] && id=$(task_for_contact "$name")

    if [ -n "$name" ] && [ -n "$id" ]; then
        handle_contact_reply "$name" "$from" "$owned" "$id" "$body"
    elif [ -n "$name" ]; then
        concierge_reply "$name" "$body" "$project" "$from" "$owned"
    elif allowed "$from"; then
        handle_message "$body" "$from" "$owned"
    else
        log "ignoring message from $from, not in ALLOWED or CONTACTS"
    fi
}

# One pass over every owned number's new inbound messages: GET
# Messages.json filtered To=<number> and DateSent>= the cursor (kept in
# $FORGE_PLUGIN_STATE/cursor), merged and processed oldest first. Every
# message actually seen is appended to
# $FORGE_PLUGIN_STATE/inbox.jsonl (for `code`, below) before it is
# routed. `$FORGE_PLUGIN_STATE/cursor-sids` names every message id
# already processed at the cursor's own timestamp, so a restart (or a
# DateSent filter no finer than a second) can never process the same
# message twice; it resets whenever the cursor itself advances.
poll_once() {
    cursor_file="$FORGE_PLUGIN_STATE/cursor"
    seen_file="$FORGE_PLUGIN_STATE/cursor-sids"
    [ -f "$cursor_file" ] || : >"$cursor_file"
    [ -f "$seen_file" ] || : >"$seen_file"
    cursor=$(cat "$cursor_file")

    doc=$(owned_json)
    owned_numbers=$(printf '%s' "$doc" | jq -r '.incoming_phone_numbers[].phone_number')
    [ -n "$owned_numbers" ] || return 0

    all_file=$(mktemp)
    printf '[]' >"$all_file"
    for num in $owned_numbers; do
        query="To=$(urlenc "$num")"
        [ -n "$cursor" ] && query="$query&DateSent%3E%3D=$(urlenc "$cursor")"
        resp_file=$(mktemp)
        api_get "Messages.json?$query" >"$resp_file"
        merged=$(mktemp)
        jq -s '.[0] + (.[1].messages // [])' "$all_file" "$resp_file" >"$merged"
        mv "$merged" "$all_file"
        rm -f "$resp_file"
    done

    jq -c 'sort_by(.date_sent)[]' "$all_file" | while IFS= read -r msg; do
        [ -z "$msg" ] && continue
        sid=$(printf '%s' "$msg" | jq -r '.sid // empty')
        [ -n "$sid" ] || continue
        grep -qxF "$sid" "$seen_file" 2>/dev/null && continue

        date_sent=$(printf '%s' "$msg" | jq -r '.date_sent // empty')
        from=$(printf '%s' "$msg" | jq -r '.from // empty')
        to=$(printf '%s' "$msg" | jq -r '.to // empty')
        body=$(printf '%s' "$msg" | jq -r '.body // ""')

        printf '%s\n' "$msg" >>"$FORGE_PLUGIN_STATE/inbox.jsonl"

        handle_inbound "$to" "$from" "$body"

        if [ "$date_sent" != "$cursor" ]; then
            cursor=$date_sent
            : >"$seen_file"
        fi
        printf '%s\n' "$sid" >>"$seen_file"
        printf '%s\n' "$cursor" >"$cursor_file"
    done
    rm -f "$all_file"
}

# The supervised entry point (no verb): polls every owned number's
# inbound messages every POLL_SECS, forever.
inbound() {
    while :; do
        poll_once
        sleep "$POLL_SECS"
    done
}

# `<number> [--since SECS] [--wait SECS]`: the most recent 4-8 digit
# code texted to $1, read from $FORGE_PLUGIN_STATE/inbox.jsonl (so this
# needs the inbound loop above already running to have anything to
# read) — a registration flow's verification code (e.g. `signal-cli
# register`), without a human reading their phone. `--since SECS`
# ignores anything older than that many seconds; `--wait SECS` polls
# for up to that long for a code to arrive before giving up. Refuses
# (exit 1) if no code is found within the wait window (or immediately,
# with none).
code() {
    number=""
    since=""
    wait_secs=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --since)
                since=$2
                shift 2
                ;;
            --wait)
                wait_secs=$2
                shift 2
                ;;
            -*)
                log "code: unknown argument $1"
                exit 1
                ;;
            *)
                number=$1
                shift
                ;;
        esac
    done
    if [ -z "$number" ]; then
        log "code: an E.164 number is required"
        exit 1
    fi

    inbox="$FORGE_PLUGIN_STATE/inbox.jsonl"
    deadline=$(($(date +%s) + wait_secs))
    while :; do
        if [ -f "$inbox" ]; then
            cutoff=0
            [ -n "$since" ] && cutoff=$(($(date +%s) - since))
            found=$(tac "$inbox" 2>/dev/null || sed '1!G;h;$!d' "$inbox")
            code_val=$(printf '%s\n' "$found" | while IFS= read -r line; do
                [ -z "$line" ] && continue
                to=$(printf '%s' "$line" | jq -r '.to // empty')
                [ "$to" = "$number" ] || continue
                ds=$(printf '%s' "$line" | jq -r '.date_sent // empty')
                epoch=$(date -u -d "$ds" +%s 2>/dev/null || printf '0')
                if [ -n "$since" ] && [ "$epoch" -lt "$cutoff" ]; then
                    continue
                fi
                body=$(printf '%s' "$line" | jq -r '.body // empty')
                c=$(printf '%s' "$body" | grep -oE '[0-9]{4,8}' | head -n1)
                if [ -n "$c" ]; then
                    printf '%s\n' "$c"
                    break
                fi
            done)
            if [ -n "$code_val" ]; then
                printf '%s\n' "$code_val"
                return 0
            fi
        fi
        [ "$(date +%s)" -ge "$deadline" ] && break
        sleep 1
    done
    log "code: no code found for $number"
    exit 1
}

verb=${1:-}
[ $# -gt 0 ] && shift

case "$verb" in
    search | owned | buy | release | "")
        if [ -z "$TWILIO_ACCOUNT_SID" ]; then
            log "TWILIO_ACCOUNT_SID is not set in $config"
            exit 1
        fi
        if [ -z "$auth_pass" ]; then
            log "no credentials: set TWILIO_AUTH_TOKEN or TWILIO_API_KEY_SID/TWILIO_API_KEY_SECRET in $config"
            exit 1
        fi
        ;;
esac

case "$verb" in
    search) search "$@" ;;
    owned) owned "$@" ;;
    buy) buy "$@" ;;
    release) release "$@" ;;
    code) code "$@" ;;
    "")
        # No verb: the supervised `run` entry point a `forge plugin
        # enable twilio` starts. Polls every owned number's inbound
        # messages, forever (docs/PLUGINS.md); buying is never reached
        # from here, gated above on a human passing --yes by hand.
        inbound
        ;;
    *)
        log "unknown verb $verb (search, owned, buy, release, code)"
        exit 1
        ;;
esac
