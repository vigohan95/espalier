#!/usr/bin/env bash
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version; cargo --version; cargo near --version; near --version; /root/tools/near-sandbox --version | head -1
