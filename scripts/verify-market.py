#!/usr/bin/env python3
"""Checks the market's testnet run without any key. Python standard library only.

    python3 scripts/verify-market.py

It looks up every recorded transaction on a public RPC and compares its reason code with the record,
reads every task and every grant back through view calls and compares them with the record, runs the
pricing rule again on the revealed bids, recomputes the invariants, and compares the code on chain
with the wasm files in res/.
"""
import base64
import hashlib
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "client"))

from espalier import market as mk  # noqa: E402
from espalier import near  # noqa: E402

MILLI = near.MILLI
RUN = os.path.join(HERE, "..", "docs", "evidence", "swarm-sim-testnet.json")
BAD = []


def check(ok, text):
    print(("  ok   " if ok else "  FAIL ") + text)
    if not ok:
        BAD.append(text)


def b58(hexdigest):
    return near.b58encode(bytes.fromhex(hexdigest))


def code_on_chain(result):
    """The reason code a transaction ended with, read from its final status."""
    status = result["status"]
    if "SuccessValue" in status:
        raw = base64.b64decode(status["SuccessValue"])
        try:
            v = json.loads(raw) if raw else None
        except ValueError:
            v = None
        return v.get("code", "ok") if isinstance(v, dict) else "ok"
    m = re.search(r"panicked: ([A-Z_]+)", json.dumps(status))
    return m.group(1) if m else "FAILED"


def main():
    run = json.load(open(RUN, encoding="utf8"))
    rpc = near.Rpc(os.environ.get("NEAR_RPC", "https://test.rpc.fastnear.com"), retries=8)
    archival = near.Rpc(os.environ.get("NEAR_ARCHIVAL_RPC", "https://archival-rpc.testnet.fastnear.com"), retries=8)
    market, vault = run["market"], run["vault"]
    short = {v: k for k, v in run["accounts"].items()}
    print(f"market {market}\nvault  {vault}\n")

    found = missing = differ = 0
    results = near.find_txs((archival, rpc), [(t["hash"], t["signer"]) for t in run["txs"]])
    for t, res in zip(run["txs"], results):
        if res is None:
            missing += 1
            print("  MISSING", t["hash"], t["what"])
            continue
        found += 1
        got = code_on_chain(res)
        # A record of "ok" means the call succeeded and returned no code of its own.
        if t["code"] not in (None, got) and not (t["code"] == "ok" and "SuccessValue" in res["status"]):
            differ += 1
            print("  DIFFERENT", t["hash"], t["what"], "recorded", t["code"], "chain", got)
    check(missing == 0 and differ == 0, f"{found} of {len(run['txs'])} recorded transactions found on chain, {missing} missing, {differ} with a different reason code")

    tasks = {}
    count = rpc.view(market, "config")["tasks"]
    for start in range(1, count + 1, 50):
        for t in rpc.view(market, "get_tasks", {"from": start, "limit": 50}):
            tasks[t["id"]] = t
    grants = {g: rpc.view(vault, "get_grant", {"grant_id": g}) for g in range(1, rpc.view(vault, "vault_info")["grants"] + 1)}

    same = 0
    for name, rec in run["tasks"].items():
        t = tasks[rec["id"]]
        now = (t["status"], short.get(t["winner"]), int(t["price"]) // MILLI, int(t["paid"]) // MILLI, int(t["refunded"]) // MILLI)
        was = (rec["status"], rec["winner"], rec["price_mnear"], rec["paid_mnear"], rec["refunded_mnear"])
        same += now == was
        if now != was:
            print("  DIFFERENT task", name, "recorded", was, "chain", now)
    check(same == len(run["tasks"]), f"{same} of {len(run['tasks'])} tasks read back from the market: same status, winner, price, paid and refunded as recorded")

    same = 0
    for name, rec in run["grants"].items():
        g = grants[rec["grant"]]
        now = (g["parent"], int(g["cap"]) // MILLI, int(g["spent"]) // MILLI, g["live"])
        was = (rec["parent"], rec["cap_mnear"], rec["spent_mnear"], rec["live"])
        same += now == was
        if now != was:
            print("  DIFFERENT grant", name, "recorded", was, "chain", now)
    check(same == len(run["grants"]), f"{same} of {len(run['grants'])} grants read back from the vault: same parent, cap, spent and live flag as recorded")

    closed = [t for t in tasks.values() if t["status"] != "Open" and not (t["status"] == "Void" and not t["winner"])]
    agree = 0
    for t in closed:
        bids = [(int(b["price"]), i) for i, b in enumerate(t["bids"]) if b["revealed"]]
        got = mk.pick(bids, int(t["spec"]["max_budget"]), t["spec"]["min_bids"])
        want = None if not t["winner"] else (next(i for i, b in enumerate(t["bids"]) if b["worker"] == t["winner"]), int(t["price"]))
        if got is not None:
            got = (bids[got[0]][1], got[1])
        agree += got == want
    check(agree == len(closed), f"winner and price recomputed from the revealed bids on {agree} of {len(closed)} closed tasks: same as the contract")

    awarded = [t for t in tasks.values() if t["grant"]]
    check(all(int(g["spent"]) + int(g["reserved"]) <= int(g["cap"]) for g in grants.values()), f"no grant spent past its cap ({len(grants)} grants)")
    check(all(int(g["spent"]) >= sum(int(grants[c]["spent"]) for c in g["children"]) for g in grants.values()), "every grant's counter covers everything spent below it")
    check(all(int(grants[t["grant"]]["cap"]) == int(t["price"]) and grants[t["grant"]]["parent"] == t["spec"]["grant_id"] for t in awarded), f"each of the {len(awarded)} awarded grants has its task's price as its cap and the poster's grant as its parent")
    check(all(grants[t["grant"]]["recipients"] == [market] for t in awarded), "every awarded grant can pay only the market")
    check(all(int(t["paid"]) <= int(t["price"]) and int(t["paid"]) + int(t["held"]) + int(t["refunded"]) <= int(grants[t["grant"]]["direct_spent"]) for t in awarded), "no task paid more than its price, or more than its winner's grant drew")
    check(all(int(t["paid"]) == 0 or t["status"] in ("Accepted", "Paid") for t in tasks.values()), "nothing was paid for work that was not accepted")
    for t in awarded:
        c = rpc.view(vault, "contact", {"grant_id": t["grant"], "recipient": market})
        if int((c or {}).get("total", 0)) != int(grants[t["grant"]]["direct_spent"]):
            BAD.append(f"contact of grant {t['grant']}")
    check(not any(b.startswith("contact") for b in BAD), "the vault's own record of what each winner's grant paid the market equals what the market counted from")
    root = next(g for g in grants.values() if g["parent"] is None and any(t["spec"]["grant_id"] == g["id"] for t in tasks.values()))
    paid = sum(int(t["paid"]) for t in tasks.values())
    back = sum(int(t["refunded"]) + int(t["held"]) for t in tasks.values())
    check(int(root["spent"]) == paid + back, f"the root was charged {int(root['spent']) // MILLI} mNEAR = {paid // MILLI} paid to workers + {back // MILLI} drawn without acceptance and sent back")

    account = rpc.view_account(market)
    s = rpc.view(market, "solvency")
    storage = account["storage_usage"] * 10**19
    check(int(account["amount"]) >= int(s["owed"]) + storage, f"the market holds {int(account['amount']) / 10**24:.3f} NEAR, owes {int(s['owed']) / 10**24:.3f}, and needs {storage / 10**24:.3f} for storage")

    for label, account_id, name in (("vault", vault, "espalier_vault"), ("market", market, "espalier_market")):
        sha = hashlib.sha256(open(os.path.join(HERE, "..", "res", f"{name}.wasm"), "rb").read()).hexdigest()
        a = rpc.view_account(account_id)
        keys = rpc.keys(account_id)
        full = sum(1 for k in keys if k["access_key"]["permission"] == "FullAccess")
        check(a["code_hash"] == b58(sha), f"{label} {account_id} runs res/{name}.wasm, sha256 {sha[:16]}...")
        print(f"       {len(keys)} keys on the account, {full} full-access")
    print(f"\n{'all checks passed' if not BAD else str(len(BAD)) + ' CHECKS FAILED'}")
    sys.exit(1 if BAD else 0)


main()
