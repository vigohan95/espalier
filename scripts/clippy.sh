#!/usr/bin/env bash
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO"
export CARGO_TARGET_DIR="$BUILD/prop-target"
timeout 600 cargo clippy -p espalier-vault --tests 2>&1 | grep -E -A6 '^(warning|error)' | cut -c1-130 | head -70
