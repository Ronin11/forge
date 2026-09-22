#!/bin/bash
# A fake `curl`, standing in for the Twilio REST and Pricing APIs
# (tests/e2e/plugins.rs, plugins/twilio/twilio.sh). $FAKE_TWILIO_DIR
# holds this fake account's state: available.json and pricing.json are
# read-only fixtures the test writes up front, owned.json is the
# account's current IncomingPhoneNumbers list, mutated here by a POST
# (buy) or DELETE (release) the same way Twilio's own API would.
set -u

dir="${FAKE_TWILIO_DIR:?FAKE_TWILIO_DIR not set}"
owned_file="$dir/owned.json"
[ -f "$owned_file" ] || echo '[]' >"$owned_file"
available_file="$dir/available.json"
pricing_file="$dir/pricing.json"
calls_file="$dir/calls.log"

method=GET
outfile=""
want_code=0
data=""
url=""

while [ $# -gt 0 ]; do
    case "$1" in
        -s | -sS)
            shift
            ;;
        -u)
            shift 2
            ;;
        -X)
            method=$2
            shift 2
            ;;
        -o)
            outfile=$2
            shift 2
            ;;
        -w)
            want_code=1
            shift 2
            ;;
        --data-urlencode)
            data=$2
            shift 2
            ;;
        *)
            url=$1
            shift
            ;;
    esac
done

printf '%s %s\n' "$method" "$url" >>"$calls_file"

# Strip scheme and host, keeping the path and query Twilio would route
# on.
path=${url#*//}
path=${path#*/}
query=""
case "$path" in
    *\?*)
        query=${path#*\?}
        path=${path%%\?*}
        ;;
esac

body=""
code=200

case "$method $path" in
    "GET "*"AvailablePhoneNumbers/"*"/Local.json")
        contains=""
        case "$query" in
            *Contains=*)
                contains=${query#*Contains=}
                contains=${contains%%&*}
                contains=${contains//%2B/+}
                ;;
        esac
        if [ -n "$contains" ]; then
            body=$(jq --arg n "$contains" '{available_phone_numbers: [.available_phone_numbers[] | select(.phone_number==$n)]}' "$available_file")
        else
            body=$(cat "$available_file")
        fi
        ;;
    "GET "*"PhoneNumbers/Countries/"*)
        body=$(cat "$pricing_file")
        ;;
    "GET "*"IncomingPhoneNumbers.json")
        case "$query" in
            *PhoneNumber=*)
                num=${query#*PhoneNumber=}
                num=${num%%&*}
                num=${num//%2B/+}
                body=$(jq --arg n "$num" '{incoming_phone_numbers: [.[] | select(.phone_number==$n)]}' "$owned_file")
                ;;
            *)
                body=$(jq '{incoming_phone_numbers: .}' "$owned_file")
                ;;
        esac
        ;;
    "POST "*"IncomingPhoneNumbers.json")
        num=${data#PhoneNumber=}
        sid="PN$(printf '%s' "$num" | md5sum | cut -c1-32)"
        tmp=$(mktemp)
        jq --arg n "$num" --arg s "$sid" '. + [{sid: $s, phone_number: $n}]' "$owned_file" >"$tmp" && mv "$tmp" "$owned_file"
        body=$(jq -n --arg n "$num" --arg s "$sid" '{sid: $s, phone_number: $n}')
        ;;
    "DELETE "*"IncomingPhoneNumbers/"*)
        sid=${path##*IncomingPhoneNumbers/}
        sid=${sid%.json}
        tmp=$(mktemp)
        jq --arg s "$sid" '[.[] | select(.sid != $s)]' "$owned_file" >"$tmp" && mv "$tmp" "$owned_file"
        body=""
        code=204
        ;;
    *)
        body="{\"error\":\"fake curl: unhandled $method $path\"}"
        code=500
        ;;
esac

if [ -n "$outfile" ]; then
    if [ "$outfile" != "/dev/null" ]; then
        printf '%s' "$body" >"$outfile"
    fi
else
    printf '%s' "$body"
fi
if [ "$want_code" = 1 ]; then
    printf '%s' "$code"
fi
