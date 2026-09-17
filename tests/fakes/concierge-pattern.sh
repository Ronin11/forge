#!/bin/bash
# the concierge directive: another quote-text change, the same shape as
# tasks 1, 2 and 3 already on the project's record, so alongside the
# ordinary request it names the pattern (see docs/INTAKE.md, "The
# escalator").
cat >/dev/null
decision='{"kind":"request","task":"Change the quote text, variant 4.","answer":"","reason":"","question":"","pattern":{"task_ids":[1,2,3],"repetition":"That is the third time you have asked to change the quote text by hand.","outcome":"Quotes pull their wording from your price sheet automatically."}}'
escaped="${decision//\\/\\\\}"
escaped="${escaped//\"/\\\"}"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"summary":"'"$escaped"'","needs_input":null,"changes":[],"checks_run":[],"claims":[]}}'
