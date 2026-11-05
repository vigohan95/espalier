#!/usr/bin/env bash
# Source-builds near-cli-rs and cargo-near detached from the launching shell, logs to /root/logs.
export PATH="$HOME/.cargo/bin:$PATH"
mkdir -p /root/logs
if ! command -v near >/dev/null; then
  nohup timeout 1800 cargo install --locked near-cli-rs >/root/logs/near-cli.log 2>&1 &
fi
if ! command -v cargo-near >/dev/null && ! pgrep -f 'cargo install --locked cargo-near' >/dev/null; then
  nohup timeout 1800 cargo install --locked cargo-near >/root/logs/cargo-near.log 2>&1 &
fi
sleep 1
pgrep -af 'cargo install' || true
