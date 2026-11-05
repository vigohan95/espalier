#!/usr/bin/env bash
# Testnet phases. Usage: tn.sh deploy|run|lock [extra harness args]
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
cd "$REPO"
export ESPALIER_WASM_DIR="$BUILD/wasm"
A=espalier-vault-1105.testnet
P="$1"; shift
timeout 1500 "$CARGO_TARGET_DIR/release/harness" testnet --account $A --key-file ~/.near-credentials/testnet/$A.json --phase "$P" "$@" 2>&1
echo "exit $?"
