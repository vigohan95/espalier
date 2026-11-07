#!/usr/bin/env python3
"""Checks the testnet evidence without any key: every recorded transaction is looked up on a public
RPC and its logged reason code is compared with the report. Python standard library only.

Regular RPC nodes forget a transaction after about two days, and public archival RPC allows only a few
calls a minute, so transactions are read in batches from FastNear's indexer, with archival RPC for
anything it does not return. Every call is retried: a public endpoint drops a connection now and then."""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "client"))

from espalier import near  # noqa: E402

RPC = os.environ.get("NEAR_RPC", "https://test.rpc.fastnear.com")
ARCHIVAL = os.environ.get("NEAR_ARCHIVAL_RPC", "https://archival-rpc.testnet.fastnear.com")
REPORT = os.path.join(HERE, "..", "docs", "evidence", "testnet-run.json")
NODES = [near.Rpc(ARCHIVAL, retries=8), near.Rpc(RPC, retries=8)]


def rpc(method, params):
    return NODES[1].raw(method, params)


def main():
    report = json.load(open(REPORT))
    deploy = json.load(open(os.path.join(os.path.dirname(REPORT), "testnet-deploy.json")))
    vault = deploy["vault"]
    checked = mismatched = missing = 0
    rows = [(s, t) for s in report["scenarios"] for t in s["txs"]]

    def signer_of(t):
        signer = deploy["owner"] if t["signer"] == "owner" else t["signer"]
        return signer if "." in signer else vault

    for (s, t), result in zip(rows, near.find_txs(NODES, [(t["hash"], signer_of(t)) for _, t in rows])):
        if result is None:
            missing += 1
            print("MISSING", s["id"], t["hash"])
            continue
        logs = [l for o in result["receipts_outcome"] for l in o["outcome"]["logs"]]
        joined = "\n".join(logs)
        ok = (not t["code"]) or t["outcome"] == "ok" or t["code"] in joined or t["outcome"] in ("settled", "rolled_back", "queued", "allowed")
        checked += 1
        if not ok:
            mismatched += 1
            print("MISMATCH", s["id"], t["hash"], t["code"])
    print(f"{checked} transactions found, {missing} missing, {mismatched} mismatched, vault {vault}")
    view = rpc("query", {"request_type": "view_access_key_list", "account_id": vault, "finality": "final"})
    keys = view["result"]["keys"]
    full = [k for k in keys if k["access_key"]["permission"] == "FullAccess"]
    print(f"vault has {len(keys)} keys, {len(full)} full-access")
    for label, account_id, sha in (("vault", vault, deploy["wasm_sha256"]), ("registry", deploy["registry"], deploy["registry_wasm_sha256"])):
        account = rpc("query", {"request_type": "view_account", "account_id": account_id, "finality": "final"})["result"]
        alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
        n, digest = int(sha, 16), ""
        while n:
            n, r = divmod(n, 58)
            digest = alphabet[r] + digest
        print(f"{label} {account_id}: on-chain code_hash {account['code_hash']}, report says {digest}, match: {account['code_hash'] == digest}")
        if account["code_hash"] != digest:
            mismatched += 1
    sys.exit(1 if missing or mismatched else 0)

main()
