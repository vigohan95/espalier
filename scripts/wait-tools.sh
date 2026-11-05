#!/usr/bin/env bash
# Exits when the background builds finished, or after 25 minutes.
for i in $(seq 1 300); do
  if ! pgrep -f 'cargo install|cargo test|cargo check' >/dev/null; then break; fi
  sleep 5
done
ls ~/.cargo/bin | grep -E 'near'
tail -3 /root/logs/cargo-near.log /root/logs/near-cli.log /root/logs/hello-compile.log
tail -40 /root/logs/check.log
