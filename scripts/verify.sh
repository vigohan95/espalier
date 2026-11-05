#!/usr/bin/env bash
cd "$(dirname "$0")/.."
timeout 200 python3 scripts/verify-testnet.py
for a in vault.espalier-vault-1105.testnet owner.espalier-vault-1105.testnet espalier-vault-1105.testnet; do
curl -sS -m 15 -X POST https://test.rpc.fastnear.com -H 'Content-Type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":\"1\",\"method\":\"query\",\"params\":{\"request_type\":\"view_account\",\"finality\":\"final\",\"account_id\":\"$a\"}}" | python3 -c "import sys,json; r=json.load(sys.stdin)['result']; print('$a', int(r['amount'])/1e24, 'NEAR', r['storage_usage'])"
done
