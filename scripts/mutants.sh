#!/usr/bin/env bash
export PATH="$HOME/.cargo/bin:$PATH"
python3 /mnt/d/income-engine/work/hackathons/near/scripts/mutants.py "${1:-4}" "${2:-1500}"
