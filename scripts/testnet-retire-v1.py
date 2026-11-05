#!/usr/bin/env python3
"""Frees the testnet NEAR locked as storage by the first vault, so the new vault and registry can be
paid for. Testnet only, and needs that vault's own key from ~/.near-credentials.

The first vault ran the contract of milestone 1, whose state layout the new contract cannot read. The
account stays, with its keys and its transaction history. Its code is replaced by a smaller contract,
which releases the storage, and the free balance goes back to the parent account."""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "client"))

from espalier import near, world  # noqa: E402

BASE = "espalier-vault-1105.testnet"
OLD = f"vault.{BASE}"
RPC = "https://test.rpc.fastnear.com"


def main():
    rpc = near.Rpc(RPC)
    data = json.load(open(os.path.expanduser(f"~/.near-credentials/testnet/{OLD}.json")))
    signer = near.Signer(rpc, OLD, near.Key.from_string(data.get("private_key") or data["secret_key"]))
    before = rpc.view_account(OLD)
    print("before", int(before["amount"]) / 1e24, before["storage_usage"])
    r = signer.deploy(world.wasm("relay"))
    print("code replaced", r.hash, r.ok)
    after = rpc.view_account(OLD)
    free = int(after["amount"]) - (after["storage_usage"] + 2000) * 10**19 - 20 * near.MILLI
    r = signer.send(BASE, [near.act_transfer(free)])
    print("returned", free / 1e24, r.hash, r.ok)
    print("parent now", int(rpc.view_account(BASE)["amount"]) / 1e24)


main()
