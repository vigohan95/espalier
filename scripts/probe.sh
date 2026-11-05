#!/usr/bin/env bash
# Network and version probe, read-only.
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version; cargo --version
for u in https://github.com https://static.crates.io https://crates.io https://test.rpc.fastnear.com/status \
         https://rpc.testnet.fastnear.com/status https://helper.nearprotocol.com https://helper.testnet.near.org \
         https://s3-us-west-1.amazonaws.com/build.nearprotocol.com/nearcore/Linux-x86_64/master/latest \
         https://near-sandbox.s3.us-west-1.amazonaws.com/ https://testnet.nearblocks.io; do
  printf '%s -> ' "$u"; timeout 15 curl -sS -o /dev/null -w '%{http_code} %{time_total}s\n' "$u" 2>&1 | tail -1
done
for c in near-sdk near-workspaces cargo-near near-cli-rs near-sandbox near-contract-standards near-api; do
  timeout 20 curl -sS -A 'near-hackathon-toolchain-probe' "https://crates.io/api/v1/crates/$c" |
    python3 -c "import sys,json; d=json.load(sys.stdin)['crate']; print(d['name'], d['max_stable_version'], d['updated_at'][:10])" 2>&1 | tail -1
done
