#!/usr/bin/env bash
# Builds the three contracts with cargo-near and collects the wasm files into res/ in the repo.
source "$(dirname "$0")/env.sh"
cd "$REPO"
set -e
mkdir -p res "$BUILD/wasm"
for pkg in contract registry test-contracts/test-ft test-contracts/relay; do
  echo "=== $pkg"
  ( cd "$pkg" && timeout 900 cargo near build non-reproducible-wasm --no-abi 2>&1 | tail -6 )
done
for f in espalier_vault espalier_registry test_ft relay; do
  w=$(find "$CARGO_TARGET_DIR/near" -name "$f.wasm" | head -1)
  cp "$w" "res/$f.wasm"
  cp "$w" "$BUILD/wasm/$f.wasm"
done
ls -la res
sha256sum res/*.wasm
