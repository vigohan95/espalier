#!/usr/bin/env bash
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO/toolchain-check"
export CARGO_TARGET_DIR="$BUILD/hello-target"
timeout 280 cargo fetch 2>&1 | tail -5
ls ~/.cargo/registry/src/*/ | grep -E '^(near-sdk|near-workspaces|near-sandbox|near-contract-standards)' | head
