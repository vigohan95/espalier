#!/usr/bin/env bash
U=https://s3-us-west-1.amazonaws.com/build.nearprotocol.com/nearcore/Linux-x86_64/2.14.0/near-sandbox.tar.gz
timeout 15 curl -sSI "$U" | grep -i content-length
for d in /root/espalier-build/target/release/build/near-sandbox-*/out/.near/*/ /root/espalier-build/hello-target/debug/build/near-sandbox-*/out/.near/*/; do ls -la --time-style=+%T $d 2>/dev/null; done
sleep 10
for d in /root/espalier-build/target/release/build/near-sandbox-*/out/.near/*/ /root/espalier-build/hello-target/debug/build/near-sandbox-*/out/.near/*/; do ls -la --time-style=+%T $d 2>/dev/null | grep tmp; done
