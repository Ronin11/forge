#!/bin/bash
# runs a read, a shell command that takes a moment, then writes the answer: tool facts to measure
cat >/dev/null
echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"'"$(pwd)"'/hello.sh"}}]}}'
echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}'
echo '{"type":"assistant","message":{"id":"m2","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"npx vitest run"}}]}}'
sleep 0.3
echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}'
echo '{"type":"assistant","message":{"id":"m3","content":[{"type":"tool_use","id":"t3","name":"Write","input":{"file_path":"answer.txt"}}]}}'
echo 42 > answer.txt && git add -A && git commit -qm "answer"
echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t3","content":"ok"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"wrote the answer","changes":[{"path":"answer.txt","kind":"added"}]}}'
