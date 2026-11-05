#!/usr/bin/env bash
# Regenerates the sandbox evidence: scenarios, gas table, then the real-runtime property runs.
S="$(dirname "$0")"
bash $S/harness.sh sandbox --report docs/evidence/sandbox-report --jobs 6 | grep -v -E ' pass |^warning|^ +-->'
bash $S/harness.sh gas --report docs/evidence/gas | grep -v -E '^warning|^ +-->'
bash $S/sprop.sh "${1:-10000}" "${2:-7}" "${3:-12}" | grep -v -E '^warning|^ +-->' | tail -60
bash $S/sprop.sh "${1:-10000}" "${2:-8}" "${3:-12}" --screened | grep -v -E '^warning|^ +-->' | tail -60
