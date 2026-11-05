#!/usr/bin/env bash
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO"
timeout 300 cargo check -p harness --release 2>&1 | grep -v '^warning: unused' | grep -E -A14 '^error' | cut -c1-150 | head -130
