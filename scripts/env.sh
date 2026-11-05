#!/usr/bin/env bash
# Shared environment for every WSL script. Target dirs live on the Linux filesystem; sources stay in the repo.
export PATH="$HOME/.cargo/bin:$PATH"
export REPO=/mnt/d/income-engine/work/hackathons/near
export BUILD=/root/espalier-build
export NEAR_SANDBOX_BIN_PATH=/root/tools/near-sandbox
export CARGO_TARGET_DIR="$BUILD/target"
mkdir -p "$BUILD" /root/logs
