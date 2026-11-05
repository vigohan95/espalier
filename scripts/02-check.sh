#!/usr/bin/env bash
# Fast compile of the contract and test contracts on the host target, then the unit tests.
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO"
timeout 900 cargo check -p espalier-vault -p test-ft -p relay --tests 2>&1 | grep -E '^(error|warning: unused)|^ +-->|^error|Finished' | head -60
timeout 900 cargo check -p espalier-vault -p test-ft -p relay --tests 2>&1 | tail -40
