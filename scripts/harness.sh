#!/usr/bin/env bash
# Runs the harness binary from the repo root. Args are passed through. Builds it first if needed.
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO"
export ESPALIER_WASM_DIR="${ESPALIER_WASM_DIR:-$BUILD/wasm}"
timeout 1200 cargo build -p harness --release 2>&1 | grep -E '^(error|warning: unused)|^ +--> harness|Finished|could not compile' | head -60
timeout "${HARNESS_TIMEOUT:-1500}" "$CARGO_TARGET_DIR/release/harness" "$@" 2>&1
echo "exit $?"
