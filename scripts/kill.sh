#!/usr/bin/env bash
pkill -f 'cargo (test|build)' ; pkill -f near-sandbox ; pkill -f release/harness ; sleep 1; pgrep -af 'cargo|rustc|sandbox' | cut -c1-100
