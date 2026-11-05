#!/usr/bin/env bash
# In-process property run: JOBS processes, each with its own seeds, merged into one report.
source "$(dirname "$0")/env.sh"
cd "$REPO"
export CARGO_TARGET_DIR="$BUILD/prop-target"
JOBS=${JOBS:-8}; SEEDS=${SEEDS:-5}; OPS=${OPS:-2500}
timeout 900 cargo test -p espalier-vault --release --no-run 2>&1 | tail -2
BIN=$(ls -t "$CARGO_TARGET_DIR"/release/deps/espalier_vault-* | grep -v '\.d$' | head -1)
mkdir -p "$BUILD/prop"; rm -f "$BUILD"/prop/*.json "$BUILD"/prop/*.out
for j in $(seq 0 $((JOBS-1))); do
  PROPERTY_SEEDS=$SEEDS PROPERTY_OPS=$OPS PROPERTY_SEED_BASE=$((j*SEEDS+1)) PROPERTY_REPORT="$BUILD/prop/$j.json" \
    timeout 2400 "$BIN" random_operations --nocapture > "$BUILD/prop/$j.out" 2>&1 &
done
wait
tail -q -n 3 "$BUILD"/prop/*.out | grep -E 'test result|panicked' | sort | uniq -c
python3 scripts/merge-property.py "$BUILD/prop" docs/evidence/property-report.json
