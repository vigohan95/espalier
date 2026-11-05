#!/usr/bin/env bash
export HARNESS_TIMEOUT=${HARNESS_TIMEOUT:-3400}
bash "$(dirname "$0")/harness.sh" sandbox-property --ops "${1:-1000}" --seed "${2:-1}" --keys "${3:-12}" --report docs/evidence/sandbox-property
