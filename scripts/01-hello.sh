#!/usr/bin/env bash
# Step 1 proof: build the hello contract with cargo-near and run a sandbox test against it.
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO/toolchain-check"
export CARGO_TARGET_DIR="$BUILD/hello-target"
MODE="${1:-all}"
if [ "$MODE" = "compile-tests" ]; then
  timeout 1500 cargo test --no-run 2>&1 | tail -15
  exit 0
fi
timeout 600 cargo near build non-reproducible-wasm --no-abi 2>&1 | tail -15
ls -la "$BUILD/hello-target/near/" 2>/dev/null
WASM=$(find "$BUILD/hello-target/near" -name '*.wasm' | head -1)
echo "wasm: $WASM ($(stat -c %s "$WASM") bytes)"
HELLO_WASM="$WASM" timeout 400 cargo test -- --nocapture 2>&1 | tail -20
