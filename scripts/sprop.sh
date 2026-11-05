#!/usr/bin/env bash
# sprop.sh [ops] [seed] [keys] [--screened]: the real-runtime property test, with or without a registry.
export HARNESS_TIMEOUT=${HARNESS_TIMEOUT:-3400}
REPORT=docs/evidence/sandbox-property
[ "$4" = "--screened" ] && REPORT=docs/evidence/sandbox-property-screened
bash "$(dirname "$0")/harness.sh" sandbox-property --ops "${1:-1000}" --seed "${2:-1}" --keys "${3:-12}" --report "$REPORT" $4
