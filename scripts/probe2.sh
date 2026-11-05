#!/usr/bin/env bash
U=https://s3-us-west-1.amazonaws.com/build.nearprotocol.com/nearcore/Linux-x86_64/2.14.0/near-sandbox.tar.gz
timeout 15 curl -sSI "$U" | head -8
timeout 20 curl -sS -r 0-2000000 -o /tmp/part.bin -w 'partial %{http_code} %{size_download}B %{speed_download}B/s\n' "$U"
ls -la /proc/108864/fd 2>/dev/null | tail -8
ss -tnp 2>/dev/null | grep -E '108864|108786' | head
ls ~/.near 2>/dev/null; ls /root/espalier-build/target/release/build/ | grep -i -E 'workspaces|sandbox'; find / -name 'near-sandbox*' -not -path '/proc/*' -not -path '/mnt/*' 2>/dev/null | head
