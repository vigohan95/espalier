#!/usr/bin/env bash
# bg.sh <logname> <script> [args...]: runs a repo script detached from the wsl client, logging to /root/logs.
name="$1"; shift
mkdir -p /root/logs
nohup setsid bash "$@" >"/root/logs/$name.log" 2>&1 < /dev/null &
echo "started $name pid $!"
