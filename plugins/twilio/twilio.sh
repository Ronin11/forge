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

verb=${1:-}
[ $# -gt 0 ] && shift

case "$verb" in
    search | owned | buy | release)
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
    "")
        # No verb: this is the supervised `run` entry point a `forge
        # plugin enable twilio` starts. Task 2 turns this into the
        # inbound SMS/voice poll loop (docs/PLUGINS.md); until then it
        # holds the process open doing nothing, since nothing in this
        # file is safe to run unattended (buying is gated above on a
        # human passing --yes by hand).
        exec sleep infinity
        ;;
    *)
        log "unknown verb $verb (search, owned, buy, release)"
        exit 1
        ;;
esac
