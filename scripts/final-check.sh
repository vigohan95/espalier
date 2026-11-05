#!/usr/bin/env bash
# The README commands, from a clean shell at the repo root. Uses the prebuilt wasm in res/.
source "$(dirname "$0")/env.sh"
cd "$REPO"
unset ESPALIER_WASM_DIR
timeout 900 cargo run --release -p harness -- sandbox --only C01,E03,B06 2>&1 | grep -v -E '^warning|^ +-->|Compiling|^ *\|'
timeout 200 python3 scripts/verify-testnet.py
