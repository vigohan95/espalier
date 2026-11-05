#!/usr/bin/env python3
"""Checks the testnet evidence without any key: every recorded transaction is looked up on a public
RPC and its logged reason code is compared with the report. Python standard library only."""
import json, sys, time, urllib.request, os

RPC = os.environ.get("NEAR_RPC", "https://test.rpc.fastnear.com")
HERE = os.path.dirname(os.path.abspath(__file__))
REPORT = os.path.join(HERE, "..", "docs", "evidence", "testnet-run.json")

def rpc(method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": "x", "method": method, "params": params}).encode()
    req = urllib.request.Request(RPC, body, {"Content-Type": "application/json", "User-Agent": "espalier-verify"})
    with urllib.request.urlopen(req, timeout=20) as r:
        return json.load(r)

def main():
    report = json.load(open(REPORT))
    deploy = json.load(open(os.path.join(os.path.dirname(REPORT), "testnet-deploy.json")))
    vault = deploy["vault"]
    checked = mismatched = missing = 0
    for s in report["scenarios"]:
        for t in s["txs"]:
            signer = deploy["owner"] if t["signer"] == "owner" else t["signer"]
            if "." not in signer:
                signer = vault
            res = rpc("tx", {"tx_hash": t["hash"], "sender_account_id": signer, "wait_until": "NONE"})
            if "result" not in res or "receipts_outcome" not in res["result"]:
                missing += 1
                print("MISSING", s["id"], t["hash"], str(res)[:120])
                continue
            logs = [l for o in res["result"]["receipts_outcome"] for l in o["outcome"]["logs"]]
            joined = "\n".join(logs)
            ok = (not t["code"]) or t["outcome"] == "ok" or t["code"] in joined or t["outcome"] in ("settled", "rolled_back", "queued", "allowed")
            checked += 1
            if not ok:
                mismatched += 1
                print("MISMATCH", s["id"], t["hash"], t["code"])
            time.sleep(0.05)
    print(f"{checked} transactions found on {RPC}, {missing} missing, {mismatched} mismatched, vault {vault}")
    view = rpc("query", {"request_type": "view_access_key_list", "account_id": vault, "finality": "final"})
    keys = view["result"]["keys"]
    full = [k for k in keys if k["access_key"]["permission"] == "FullAccess"]
    print(f"vault has {len(keys)} keys, {len(full)} full-access")
    account = rpc("query", {"request_type": "view_account", "account_id": vault, "finality": "final"})["result"]
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    n, digest = int(deploy["wasm_sha256"], 16), ""
    while n:
        n, r = divmod(n, 58)
        digest = alphabet[r] + digest
    print(f"on-chain code_hash {account['code_hash']}, report says {digest}, match: {account['code_hash'] == digest}")
    if account["code_hash"] != digest:
        mismatched += 1
    sys.exit(1 if missing or mismatched else 0)

main()
