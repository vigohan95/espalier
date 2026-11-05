#!/usr/bin/env bash
export PATH="$HOME/.cargo/bin:$PATH"
python3 "$(dirname "$0")/mutants.py" "${1:-4}" "${2:-1500}"
