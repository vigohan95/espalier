#!/usr/bin/env bash
bash "$(dirname "$0")/harness.sh" gas --report docs/evidence/gas | grep -v -E '^warning|^ +-->'
