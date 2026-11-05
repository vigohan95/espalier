#!/usr/bin/env bash
# run.sh <logname> <script> [args...]: runs a script with all output in .logs/<logname>.log in the repo,
# ending with a "finished" line. The Windows wsl client can drop its pipe, so nothing relies on stdout.
name="$1"; shift
log=/mnt/d/income-engine/work/hackathons/near/.logs/$name.log
: > "$log"
nohup setsid bash -c 'bash "$@" >> "'"$log"'" 2>&1; echo "finished rc=$?" >> "'"$log"'"' _ "$@" > /dev/null 2>&1 < /dev/null &
sleep 2
pgrep -f "$1" >/dev/null && echo "started $name" || echo "NOT RUNNING $name"
