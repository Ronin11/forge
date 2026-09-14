#!/bin/bash
# a supervisor whose run dies before any ruling
cat >/dev/null
echo "supervisor crashed" >&2
exit 1
