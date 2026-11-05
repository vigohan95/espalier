#!/usr/bin/env bash
# await.sh <logname>: blocks on the Windows side until the log has its finished line, then shows the tail.
f="$(dirname "$0")/../.logs/$1.log"
until grep -aq '^finished rc=' "$f" 2>/dev/null; do sleep 3; done
tail -n "${2:-40}" "$f" | cut -c1-240
