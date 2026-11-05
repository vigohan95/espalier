#!/usr/bin/env bash
# Installs the whole NEAR toolchain inside WSL. Idempotent. Every step has a timeout.
set -u
export PATH="$HOME/.cargo/bin:$PATH"
log() { printf '\n=== %s\n' "$*"; }

log "apt deps"
timeout 300 apt-get install -y -qq build-essential pkg-config libssl-dev libudev-dev curl ca-certificates git clang >/dev/null 2>&1
echo "apt exit $?"

log "rustup"
if ! command -v rustup >/dev/null; then
  timeout 300 bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable'
fi
timeout 300 rustup target add wasm32-unknown-unknown
rustc --version
cargo --version

log "crate versions on crates.io"
for c in near-sdk near-workspaces cargo-near near-cli-rs near-sandbox-utils near-sandbox; do
  timeout 30 curl -sS -A 'near-hackathon-toolchain-probe' "https://crates.io/api/v1/crates/$c" |
    python3 -c "import sys,json; d=json.load(sys.stdin)['crate']; print(d['name'], d['max_stable_version'], d['updated_at'])" 2>&1 | tail -1
done

log "cargo-near + near-cli-rs (prebuilt installers, source build as fallback)"
if ! command -v cargo-near >/dev/null; then
  timeout 120 bash -c 'curl --proto "=https" --tlsv1.2 -LsSf https://github.com/near/cargo-near/releases/latest/download/cargo-near-installer.sh | sh' \
    || timeout 1500 cargo install --locked cargo-near
fi
if ! command -v near >/dev/null; then
  timeout 120 bash -c 'curl --proto "=https" --tlsv1.2 -LsSf https://github.com/near/near-cli-rs/releases/latest/download/near-cli-rs-installer.sh | sh' \
    || timeout 1500 cargo install --locked near-cli-rs
fi
cargo near --version
near --version

log "done"
