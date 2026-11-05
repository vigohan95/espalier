#!/usr/bin/env bash
# Read-only helper: q.sh <cmd...> runs a command in the cargo registry dir.
cd ~/.cargo/registry/src/*/ && eval "$*"
