#!/bin/bash
# the document step done right: a comment and a doc, nothing else
cat >/dev/null
sed -i '1a # prints a greeting' hello.sh
mkdir -p docs && printf '# Notes\n\nhello.sh prints a greeting.\n' > docs/NOTES.md
git add -A && git commit -qm "document the greeting"
echo '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.01,"result":"done","structured_output":{"schema_version":1,"needs_input":null,"checks_run":[],"claims":[],"summary":"documented","changes":[{"path":"hello.sh","kind":"modified"},{"path":"docs/NOTES.md","kind":"added"}]}}'
