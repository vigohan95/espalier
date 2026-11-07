#!/usr/bin/env python3
"""Gives the testnet NEAR of an abandoned run back to its base account. Testnet only.

    python3 scripts/testnet-reclaim.py BASE.testnet [--tag TAG]

A swarm run that stops half way leaves about 12 NEAR in the sub-accounts it created. Their keys are
beside the base account's key in ~/.near-credentials/testnet. Contract accounts first get their
code replaced by an empty module, which frees the storage, and then every account is deleted with
the base account as beneficiary. Nothing here touches an account that a finished run reports.
"""
import glob
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "client"))

from espalier import near  # noqa: E402

EMPTY_WASM = bytes([0, 0x61, 0x73, 0x6D, 1, 0, 0, 0])


def main():
    base = sys.argv[1]
    if not base.endswith(".testnet"):
        sys.exit("testnet accounts only")
    tag = sys.argv[sys.argv.index("--tag") + 1] if "--tag" in sys.argv else ""
    rpc = near.Rpc("https://test.rpc.fastnear.com")
    rpc.finality = "optimistic"
    home = os.path.expanduser("~/.near-credentials/testnet")
    before = int(rpc.view_account(base)["amount"])
    for path in sorted(glob.glob(os.path.join(home, f"*{tag}.{base}.json"))):
        data = json.load(open(path))
        account = data["account_id"]
        info = rpc.view_account(account)
        if info is None:
            os.remove(path)
            continue
        signer = near.Signer(rpc, account, near.Key.from_string(data["private_key"]))
        if info["code_hash"] != "1" * 32:
            r = signer.deploy(EMPTY_WASM)
            print(f"{account}: code replaced, {'ok' if r.ok else r.failure()}")
        r = signer.send(account, [near.act_delete_account(base)])
        print(f"{account}: {int(info['amount']) / 1e24:.3f} NEAR, deleted {'ok' if r.ok else r.failure()}")
        if r.ok:
            os.remove(path)
    after = int(rpc.view_account(base)["amount"])
    print(f"{base}: {before / 1e24:.3f} -> {after / 1e24:.3f} NEAR")


main()
