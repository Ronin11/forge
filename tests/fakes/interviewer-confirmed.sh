#!/bin/bash
# The interview directive on a two-workflow brief, confirmed after one
# question: turn 0 asks the person (nate) to confirm, addressed to them;
# turn 1 (once they answer) re-emits the same brief confirmed. Used by
# the intake step 3 e2e suite, which only needs a confirmed brief to
# accept, not a full multi-turn checklist (that is covered by
# interviewer.sh). The where_it_runs text names both a host and a method
# `forge intake accept` already supports.
source "$(dirname "$0")/lib.sh"
prompt="$(cat)"
n=$(grep -o "answer to a question from an earlier attempt" <<<"$prompt" | wc -l)

brief='{"workflows":[{"name":"quote by photo","trigger":"a customer texts a photo of the job","inputs":"the photo","outputs":"a quote texted back, an entry in the book","other_people":"none","failure_today":"sometimes forgets to write it in the book","success_signal":"never forgets an entry","do_not_touch":"the book stays a physical notebook"},{"name":"weekly invoice","trigger":"friday afternoon","inputs":"the week'"'"'s job entries","outputs":"an invoice emailed to each customer","other_people":"the bookkeeper","failure_today":"some weeks he forgets and does it late","success_signal":"every customer gets one by friday evening","do_not_touch":"the bookkeeper still reviews before it sends"}],"where_it_runs":"the laptop in the shop; it is local, so deploy-command should do it","do_not_touch":["the book stays a physical notebook","the bookkeeper still reviews before it sends"]'

if [ "$n" = "0" ]; then
  unconfirmed="$brief"',"confirmed":false}'
  escaped="${unconfirmed//\\/\\\\}"
  escaped="${escaped//\"/\\\"}"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":{"tried":"asked about every workflow on the checklist","question":"Here is what I have: quote by photo, and a weekly invoice, running on the shop laptop. Does that sound right?","options":[],"context":"","checkpoint":null,"to":"nate"},"changes":[],"checks_run":[],"claims":[]}}'
else
  confirmed="$brief"',"confirmed":true}'
  escaped="${confirmed//\\/\\\\}"
  escaped="${escaped//\"/\\\"}"
  echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
fi
