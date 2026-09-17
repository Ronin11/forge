#!/bin/bash
# the deploy-look directive: returns a blocking finding naming a
# placeholder image, for the e2e test asserting a blocking finding fails
# the deploy and names itself in the question.
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"not ok","structured_output":{"ok":false,"findings":[{"severity":"blocking","finding":"the map is a placeholder image, not the real map."}]}}'
