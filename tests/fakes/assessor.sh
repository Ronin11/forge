#!/bin/bash
# the assess directive: reads, changes nothing, scores the landed diff
cat >/dev/null
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.02,"result":"assessed","structured_output":{"score":7,"findings":[{"path":"answer.txt","finding":"the value 42 is a magic number with no explanation.","severity":"notable"}]}}'
