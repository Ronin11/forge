#!/bin/bash
# The interview directive across a scripted four-turn conversation with a
# contact named "nate": three plain questions (the last naming a repeated
# workflow back for a yes-or-no), then the brief plus a confirmation
# question, then success once the person confirms. The turn number is read
# from how many prior answers the growing task text already carries,
# exactly what a real multi-turn interview (one task per turn) looks like.
source "$(dirname "$0")/lib.sh"
prompt="$(cat)"
n=$(grep -o "answer to a question from an earlier attempt" <<<"$prompt" | wc -l)

question() {
  local q="$1"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"asked one plain question","needs_input":{"tried":"read the conversation so far and asked nothing else","question":"'"$q"'","options":[],"context":"","checkpoint":null,"to":"nate"},"changes":[],"checks_run":[],"claims":[]}}'
}

case "$n" in
0)
  question "What did the last customer send you?"
  ;;
1)
  question "What did you do right after that?"
  ;;
2)
  question "So every time a customer sends a photo of the job, you save it, text them a quote, and write it in the book. Is that right?"
  ;;
3)
  brief='{"workflows":[{"name":"quote by photo","trigger":"a customer texts a photo of the job","inputs":"the photo","outputs":"a quote texted back, an entry in the book","other_people":"none","failure_today":"sometimes forgets to write it in the book","success_signal":"never forgets an entry","do_not_touch":"the book stays a physical notebook"}],"where_it_runs":"his phone","do_not_touch":["the book stays a physical notebook"],"confirmed":false}'
  escaped="${brief//\\/\\\\}"
  escaped="${escaped//\"/\\\"}"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":{"tried":"asked about every workflow on the checklist","question":"Here is what I have: every time a customer texts a photo of the job, you text back a quote and write it in the book, on your phone. Does that sound right?","options":[],"context":"","checkpoint":null,"to":"nate"},"changes":[],"checks_run":[],"claims":[]}}'
  ;;
*)
  if ! grep -q "The brief so far" <<<"$prompt"; then
    echo "expected the confirming turn's brief to be carried forward" >&2
    exit 1
  fi
  brief='{"workflows":[{"name":"quote by photo","trigger":"a customer texts a photo of the job","inputs":"the photo","outputs":"a quote texted back, an entry in the book","other_people":"none","failure_today":"sometimes forgets to write it in the book","success_signal":"never forgets an entry","do_not_touch":"the book stays a physical notebook"}],"where_it_runs":"his phone","do_not_touch":["the book stays a physical notebook"],"confirmed":true}'
  escaped="${brief//\\/\\\\}"
  escaped="${escaped//\"/\\\"}"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
  ;;
esac
