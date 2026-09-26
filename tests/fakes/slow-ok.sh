#!/bin/bash
# ok.sh, after a pause long enough for a test to act while it runs
sleep 8
exec "$(dirname "$0")/ok.sh"
