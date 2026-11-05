#!/usr/bin/env bash
# Shared environment for the scripts. Target dirs live on the Linux filesystem; sources stay in the repo.
export PATH="$HOME/.cargo/bin:$PATH"
export REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export BUILD="${ESPALIER_BUILD:-$HOME/espalier-build}"
[ -x "$HOME/tools/near-sandbox" ] && export NEAR_SANDBOX_BIN_PATH="$HOME/tools/near-sandbox"
export CARGO_TARGET_DIR="$BUILD/target"
mkdir -p "$BUILD"
