#!/usr/bin/env bash
# Builds the harness into its own target so a running property test keeps its binary.
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO"
cp "$CARGO_TARGET_DIR/release/harness" "$BUILD/harness-running" 2>/dev/null
timeout 900 cargo build -p harness --release 2>&1 | grep -E '^error|Finished|could not' | head
bash scripts/tn.sh "$@"
