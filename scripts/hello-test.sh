#!/usr/bin/env bash
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO/toolchain-check"
export CARGO_TARGET_DIR="$BUILD/hello-target"
HELLO_WASM="$BUILD/hello-target/near/hello_check.wasm" timeout 1200 cargo test -- --nocapture
