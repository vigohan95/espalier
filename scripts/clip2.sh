#!/usr/bin/env bash
source /mnt/d/income-engine/work/hackathons/near/scripts/env.sh
timeout 200 rustup component add clippy 2>&1 | tail -1
bash /mnt/d/income-engine/work/hackathons/near/scripts/clippy.sh
