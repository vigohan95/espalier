#!/usr/bin/env bash
# Creates a fresh testnet account through the faucet helper. The key goes to ~/.near-credentials only.
export PATH="$HOME/.cargo/bin:$PATH"
NAME="${1:?account name}"
CFG="$HOME/.config/near-cli/config.toml"
[ -f "$CFG" ] || timeout 20 near config show-connections >/dev/null 2>&1
python3 - "$CFG" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
head, sep, rest = s.partition('[network_connection.testnet]')
if sep:
    block_end = rest.find('[network_connection.', 1)
    block = rest if block_end < 0 else rest[:block_end]
    new = re.sub(r'rpc_url = ".*"', 'rpc_url = "https://test.rpc.fastnear.com/"', block)
    s = head + sep + new + ('' if block_end < 0 else rest[block_end:])
    open(p, 'w').write(s)
PY
sed -n '/\[network_connection.testnet\]/,/^$/p' "$CFG" | head -14
timeout 90 near account create-account sponsor-by-faucet-service "$NAME" autogenerate-new-keypair save-to-legacy-keychain network-config testnet create 2>&1 | tail -12
ls -la ~/.near-credentials/testnet/ 2>/dev/null | grep -c json
timeout 30 curl -sS -X POST https://test.rpc.fastnear.com -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":\"1\",\"method\":\"query\",\"params\":{\"request_type\":\"view_account\",\"finality\":\"final\",\"account_id\":\"$NAME\"}}" | cut -c1-300
