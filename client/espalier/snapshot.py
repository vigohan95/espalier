"""Reads everything the live page shows, through the same view calls the page makes, and keeps
the answers by call. The page falls back to this file when no RPC answers, and says so."""
import json
import os
import time


def key(account, method, args):
    return f"{account}|{method}|{json.dumps(args, sort_keys=True, separators=(',', ':'))}"


def take(rpc, market, vault, names=None):
    calls = {}

    def view(account, method, args=None):
        args = args or {}
        calls[key(account, method, args)] = out = rpc.view(account, method, args)
        return out

    config = view(market, "config")
    view(market, "solvency")
    view(market, "get_roster", {"vault": vault})
    tasks = []
    for start in range(1, config["tasks"] + 1, 50):
        tasks += view(market, "get_tasks", {"from": start, "limit": 50})
    info = view(vault, "vault_info")
    posters = set()
    for gid in range(1, info["grants"] + 1):
        view(vault, "get_grant", {"grant_id": gid})
    for t in tasks:
        posters.add(t["spec"]["grant_id"])
    for gid in sorted(posters):
        view(market, "get_poster", {"vault": vault, "grant_id": gid})
    accounts = {}
    for account in (market, vault):
        a = rpc.view_account(account)
        keys = rpc.keys(account)
        accounts[account] = {
            "code_hash": a["code_hash"],
            "amount": a["amount"],
            "storage_usage": a["storage_usage"],
            "keys": len(keys),
            "full_access_keys": sum(1 for k in keys if k["access_key"]["permission"] == "FullAccess"),
        }
    return {
        "taken_at": time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime()),
        "market": market,
        "vault": vault,
        "names": names or {},
        "accounts": accounts,
        "calls": calls,
    }


def write_js(path, snap):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w", encoding="utf8", newline="\n") as f:
        f.write("// Written by client/espalier/snapshot.py. The page uses it only when no RPC answers.\n")
        f.write("window.ESPALIER_SNAPSHOT = " + json.dumps(snap, separators=(",", ":")) + ";\n")
