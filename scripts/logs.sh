#!/usr/bin/env bash
# logs.sh <lines> <name...>: tails logs and shows what is still running.
n="$1"; shift
for f in "$@"; do echo "--- $f"; tail -n "$n" "/root/logs/$f.log" 2>/dev/null; done
echo "--- running: $(pgrep -f 'cargo|rustc' -c)"; ls ~/.cargo/bin | grep -E 'near' | tr '\n' ' '; echo
