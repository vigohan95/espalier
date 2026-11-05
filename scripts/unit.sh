#!/usr/bin/env bash
# Unit tests and the in-process property test. Env PROPERTY_SEEDS / PROPERTY_OPS / PROPERTY_REPORT.
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO"
export CARGO_TARGET_DIR="$BUILD/prop-target"
timeout 1500 cargo test -p espalier-vault --release -- --nocapture 2>&1
