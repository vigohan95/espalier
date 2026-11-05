#!/usr/bin/env bash
bash /mnt/d/income-engine/work/hackathons/near/scripts/harness.sh gas --report docs/evidence/gas | grep -v -E '^warning|^ +-->'
