#!/usr/bin/env python3
"""Feasibility probe for the market design in docs/SWARM-RULES.md. Runs the committed vault wasm
(res/espalier_vault.wasm, unchanged) on a real local node, next to a throwaway contract, and checks
each assumption the design depends on. Nothing here is the market.

    NEAR_SANDBOX_BIN_PATH=~/tools/near-sandbox python3 docs/evidence/market-probe/probe.py <probe.wasm> [out.json]

Build the probe wasm first: (cd docs/evidence/market-probe && cargo near build non-reproducible-wasm --no-abi)
"""
import hashlib
import json
import os
import sys
import time

REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))
PROBE_WASM = sys.argv[1]
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(REPO, "docs", "evidence", "market-probe.json")
sys.path.insert(0, REPO + "/client")

from espalier import near, sandbox, world  # noqa: E402

MILLI = near.MILLI
RESULTS = []


def check(name, ok, detail=""):
    RESULTS.append({"check": name, "pass": bool(ok), "detail": str(detail)})
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail else ""), flush=True)


def settle(rpc, blocks=3):
    """Views read final state, which trails the last transaction by a couple of blocks."""
    h = rpc.block()["header"]["height"]
    while rpc.block()["header"]["height"] < h + blocks:
        time.sleep(0.3)


def message(probe, vault, grant, payload):
    return b"espalier-market-probe-v1" + near.bstr(probe) + near.bstr(vault) + near.u64(grant) + near.bstr(payload)


def subcontracting(w, rpc, vault, market, sb):
    """The assumptions of the subcontracting market: a chain of grants that can pay only the market,
    built level by level with the vault's own `delegate`, read back through its views."""

    def terms(now, cap, depth, methods, nodes, expires):
        return {
            "asset": "Native", "cap": str(cap), "per_tx": str(cap), "window_cap": str(cap), "window_secs": 3600,
            "approval_threshold": str(cap), "recipients": [market], "methods": methods, "expires_at_ms": expires,
            "max_depth": depth, "max_denials": 20, "key_allowance": str(200 * MILLI),
            "gas_budget": str(nodes * 250 * MILLI), "node_budget": nodes, "clearance_required": False,
        }

    def delegate(parent, key, t):
        r = parent.call(vault, "delegate", {"child_pk": key.public_str, "terms": t}, tgas=100)
        return r.value or {}, r

    def draw(signer, amount):
        r = signer.call(vault, "request_spend", {"recipient": market, "amount": str(amount), "memo": "draw"}, tgas=120)
        return r.value or {}, r

    now = w.now_ms()
    end = now + 3_600_000
    work = ["request_spend", "delegate", "revoke"]
    gc_key = near.Key.generate()
    r = w.owner.call(vault, "create_root", {"pk": gc_key.public_str, "terms": terms(now, 300 * MILLI, 3, ["delegate", "revoke"], 12, end)}, tgas=100)
    narrow_id = (r.value or {}).get("new_grant")
    narrow = near.Signer(rpc, vault, gc_key)
    d, _ = delegate(narrow, near.Key.generate(), terms(now, 120 * MILLI, 2, work, 6, end))
    check("S1 a root without request_spend cannot hand it to a child: methods only narrow", narrow_id is not None and d.get("code") == "ATT_METHODS", d.get("code"))
    gc_key = near.Key.generate()
    r = w.owner.call(vault, "create_root", {"pk": gc_key.public_str, "terms": terms(now, 300 * MILLI, 3, work, 12, end)}, tgas=100)
    gc_id = (r.value or {}).get("new_grant")
    check("S1 a root whose money can only go to the market", gc_id is not None, (r.value or {}).get("code"))
    gc = near.Signer(rpc, vault, gc_key)

    k1, k2, k3, k2b = (near.Key.generate() for _ in range(4))
    d, r = delegate(gc, k1, terms(now, 120 * MILLI, 2, work, 6, end))
    l1 = d.get("new_grant")
    check("S2 the root awards a level-1 grant with the cap of a price", l1 is not None, f"{d.get('code')}, gas {r.gas_burnt / 1e12:.2f} Tgas")
    s1 = near.Signer(rpc, vault, k1)
    d, _ = delegate(s1, k2, terms(now, 50 * MILLI, 1, work, 2, end))
    l2 = d.get("new_grant")
    s2 = near.Signer(rpc, vault, k2)
    d, _ = delegate(s2, k3, terms(now, 20 * MILLI, 0, ["request_spend"], 0, end))
    l3 = d.get("new_grant")
    s3 = near.Signer(rpc, vault, k3)
    check("S3 the winner awards its own sub-grant, and that one awards a third level", l2 is not None and l3 is not None, f"grants {gc_id} > {l1} > {l2} > {l3}")
    settle(rpc)

    g = rpc.view(vault, "get_grant", {"grant_id": l2})
    seen = (g["parent"], g["pk"], g["cap"], g["per_tx"], g["window_cap"], g["approval_threshold"], g["recipients"], g["clearance_required"], g["max_depth"], g["nodes_left"], g["live"])
    want = (l1, k2.public_str, str(50 * MILLI), str(50 * MILLI), str(50 * MILLI), str(50 * MILLI), [market], False, 1, 1, True)
    check("S4 get_grant shows everything a market must confirm about an award: parent, key, cap, limits, recipients", seen == want, f"parent {g['parent']}, cap {int(g['cap']) / MILLI} mNEAR, nodes left {g['nodes_left']}")
    check("S4 get_lineage walks from the third level to the root", rpc.view(vault, "get_lineage", {"grant_id": l3}) == [l3, l2, l1, gc_id])
    check("S4 grant_of_key maps a bound key to its grant, and an unused key to nothing", rpc.view(vault, "grant_of_key", {"pk": k3.public_str}) == l3 and rpc.view(vault, "grant_of_key", {"pk": near.Key.generate().public_str}) is None)

    d, _ = delegate(s1, k2, terms(now, 10 * MILLI, 0, ["request_spend"], 0, end))
    check("S5 a key that already belongs to a grant cannot be bound to a second one", d.get("code") == "KEY_IN_USE", d.get("code"))
    d, _ = delegate(s2, near.Key.generate(), terms(now, 60 * MILLI, 0, ["request_spend"], 0, end))
    check("S5 an award larger than the awarding grant has left is refused by the vault", d.get("code") == "ATT_CAP", d.get("code"))
    wide = terms(now, 10 * MILLI, 0, ["request_spend"], 0, end)
    wide["recipients"] = [market, "relayer." + sb.root.account]
    d, _ = delegate(s2, near.Key.generate(), wide)
    check("S5 a sub-grant cannot add a recipient beside the market", d.get("code") == "ATT_RECIPIENTS", d.get("code"))

    d, r = draw(s3, 20 * MILLI)
    settle(rpc)
    spent = [int(rpc.view(vault, "get_grant", {"grant_id": i})["spent"]) // MILLI for i in (l3, l2, l1, gc_id)]
    check("S6 a third-level draw settles and is charged to every level up to the root", d.get("outcome") == "settled" and spent == [20, 20, 20, 20], f"spent by level {spent} mNEAR, gas {r.gas_burnt / 1e12:.2f} Tgas")
    c3 = rpc.view(vault, "contact", {"grant_id": l3, "recipient": market})
    c2 = rpc.view(vault, "contact", {"grant_id": l2, "recipient": market})
    check("S6 contact credits the draw to the grant that made it, not to its parent", c3["total"] == str(20 * MILLI) and c2 is None, f"L3 {int(c3['total']) / MILLI} mNEAR, L2 {c2}")
    d, _ = draw(s3, 1 * MILLI)
    check("S6 a grant cannot draw past its own price", d.get("code") == "CAP", d.get("code"))
    r = s3.call(vault, "request_spend", {"recipient": "relayer." + sb.root.account, "amount": str(MILLI)}, tgas=120)
    check("S6 a hijacked sub-contractor cannot pay an outside account", (r.value or {}).get("code") == "RECIPIENT_NOT_ALLOWED", (r.value or {}).get("code"))
    d, _ = draw(gc, 5 * MILLI)
    settle(rpc)
    root = rpc.view(vault, "get_grant", {"grant_id": gc_id})
    croot = rpc.view(vault, "contact", {"grant_id": gc_id, "recipient": market})
    check("S6 direct_spent is what a grant paid itself, also for a root, where contact adds the tree", d.get("outcome") == "settled" and root["direct_spent"] == str(5 * MILLI) and root["spent"] == str(25 * MILLI) and croot["total"] == str(25 * MILLI), f"root direct {int(root['direct_spent']) / MILLI}, spent {int(root['spent']) / MILLI}, contact {int(croot['total']) / MILLI} mNEAR")
    g3 = rpc.view(vault, "get_grant", {"grant_id": l3})
    check("S6 for a grant whose only recipient is the market, direct_spent equals its contact total", g3["direct_spent"] == c3["total"] == str(20 * MILLI))

    # Two sub-contractors under one contractor, together promised more than it has left.
    d, _ = delegate(s1, k2b, terms(now, 100 * MILLI, 0, ["request_spend"], 0, end))
    l2b = d.get("new_grant")
    check("S7 a second award up to the contractor's remainder is allowed: caps are not carved out", l2b is not None, d.get("code"))
    s2b = near.Signer(rpc, vault, k2b)
    d, _ = draw(s2b, 100 * MILLI)
    check("S7 the first sibling draws its whole price", d.get("outcome") == "settled", d.get("code"))
    d, _ = draw(s2, 30 * MILLI)
    check("S7 the other sibling's draw is refused at the shared parent", d.get("code") == "ANCESTOR_CAP" and d.get("blocking_grant") == l1, f"{d.get('code')} blocking grant {d.get('blocking_grant')}")
    settle(rpc)

    # Revoking the contractor cuts every level below it in one transaction.
    r = gc.call(vault, "revoke", {"grant_id": l1}, tgas=100)
    settle(rpc)
    live = [rpc.view(vault, "get_grant", {"grant_id": i})["live"] for i in (gc_id, l1, l2, l3, l2b)]
    check("S8 one revoke by the root makes the contractor and all three grants below it not live", (r.value or {}).get("code") == "REVOKED" and live == [True, False, False, False, False], f"live {live}, gas {r.gas_burnt / 1e12:.2f} Tgas")
    keys = {k["public_key"] for k in rpc.keys(vault)}
    check("S8 and deletes their keys in the same transaction", not ({k1.public_str, k2.public_str, k3.public_str, k2b.public_str} & keys) and gc_key.public_str in keys)
    root = rpc.view(vault, "get_grant", {"grant_id": gc_id})
    check("S8 the root's counters keep exactly what was drawn before the revoke", root["spent"] == str(125 * MILLI), f"{int(root['spent']) / MILLI} mNEAR of 300")


def main():
    with sandbox.Sandbox(port=3071) as sb:
        w = world.World(sb.rpc, sb.root).build()  # vault with registry screen and attestor, as on testnet
        rpc = sb.rpc
        vault = w.vault_id
        vault_sha = hashlib.sha256(world.wasm("espalier_vault")).hexdigest()
        print("vault wasm sha256", vault_sha)

        probe = w.account("market", 5)
        world.must(probe.deploy(open(PROBE_WASM, "rb").read()), "deploy probe")
        relayer = w.account("relayer", 2)
        market = probe.account

        # Tree: root (cap 150) -> A (cap 100, may pay only the market), B (cap 100, same).
        root = w.root_agent(150 * MILLI, 150 * MILLI, clearance_required=False)
        key_a, key_b = near.Key.generate(), near.Key.generate()
        t = root.child_terms(100 * MILLI, 100 * MILLI, [market], methods=("request_spend",))
        da, _ = root.hire(key_a.public_str, t)
        db, _ = root.hire(key_b.public_str, t)
        a = w.worker(da["new_grant"], key_a)
        b = w.worker(db["new_grant"], key_b)
        check("setup: root and two bidder grants exist", a.grant and b.grant, f"root {root.grant}, A {a.grant}, B {b.grant}")

        def ask(grant, key, payload="hello", signer_key=None):
            sig = (signer_key or key).sign(message(market, vault, grant, payload)).hex()
            r = relayer.call(market, "check", {"vault": vault, "grant_id": grant, "payload": payload, "signature": sig}, tgas=60)
            return r

        # P1/P3: before any payment, the probe sees the grant, its key and its allowlist.
        r = ask(a.grant, key_a)
        v = r.value
        check("P1 cross-contract get_grant + contact answer in one callback", r.ok and v and v["grant_found"], f"gas burnt {r.gas_burnt / 1e12:.2f} Tgas, GrantView {v['grant_view_bytes']} bytes")
        check("P3 signature by the grant key verifies on chain against the pk the vault returns", v["sig_ok"])
        check("P1 allowlist is visible to the probe and contains it", v["probe_in_allowlist"] is True)
        check("P1 nothing credited before any payment", v["contact_total"] == "0" and v["credit"] == "0")
        gas_check = r.gas_burnt
        r = ask(a.grant, key_a, signer_key=key_b)
        check("P3 signature by another grant's key is rejected", r.value["sig_ok"] is False)
        r = ask(a.grant, key_a, payload="other")
        sig_other = key_a.sign(message(market, vault, a.grant, "hello")).hex()
        r2 = relayer.call(market, "check", {"vault": vault, "grant_id": a.grant, "payload": "other", "signature": sig_other}, tgas=60)
        check("P3 a signature does not carry over to a different payload", r.value["sig_ok"] is True and r2.value["sig_ok"] is False)

        # The mandate check: the market can ask a vault who owns it and compare with its caller.
        r = w.owner.call(market, "owner_of", {"vault": vault}, tgas=30)
        check("P1 cross-contract vault_info tells the probe that the caller is the vault's owner", r.ok and r.value == [w.owner.account, True], r.value)
        r = relayer.call(market, "owner_of", {"vault": vault}, tgas=30)
        check("P1 and that another caller is not", r.ok and r.value == [w.owner.account, False], r.value)

        # A buyer can ask the vault, for free, what a deposit would do before it spends any gas.
        pv = rpc.view(vault, "preview_spend", {"grant_id": a.grant, "recipient": market, "amount": str(60 * MILLI)})
        check("P2 preview_spend says a deposit that fits would go through", pv["outcome"] == "allowed", pv["code"])
        pv = rpc.view(vault, "preview_spend", {"grant_id": a.grant, "recipient": market, "amount": str(101 * MILLI)})
        check("P2 preview_spend says a deposit over the limits would be refused", pv["outcome"] == "denied", pv["code"])

        # P2: the agent pays the market with its vault key. Plain transfer, no call on the market.
        before = w.balance(market)
        r = a._spend(market, 60 * MILLI, "round 1 deposit")
        d = r.value or {}
        settle(rpc)
        after = w.balance(market)
        check("P2 request_spend to a contract account settles (registry screen on)", d.get("outcome") == "settled", f"{d.get('outcome')} {d.get('code')}, gas {r.gas_burnt / 1e12:.2f} Tgas")
        check("P2 market balance rose by exactly the deposit", after - before == 60 * MILLI, f"{(after - before) / MILLI} mNEAR")
        c = rpc.view(vault, "contact", {"grant_id": a.grant, "recipient": market})
        check("P2 vault.contact(grant, market) records it", c and c["count"] == 1 and c["total"] == str(60 * MILLI), c)

        r = ask(a.grant, key_a)
        v = r.value
        check("P2 probe credits exactly the settled amount", v["newly_credited"] == str(60 * MILLI) and v["credit"] == str(60 * MILLI), v["credit"])
        check("P1 spendable seen by the probe is the lineage minimum", v["spendable"] == str(40 * MILLI), f"{int(v['spendable']) / MILLI} mNEAR (A has 40 left, root has 90)")
        r = ask(a.grant, key_a)
        check("P2 the same payment is never credited twice", r.value["newly_credited"] == "0" and r.value["credit"] == str(60 * MILLI))
        r = ask(b.grant, key_b)
        check("P2 a grant that paid nothing gets no credit", r.value["credit"] == "0" and r.value["contact_total"] == "0")

        # Budget competition: B's deposit hits the shared ancestor's cap, in the vault, before the market.
        r = b._spend(market, 100 * MILLI, "too much for the tree")
        d = r.value or {}
        check("P2 sibling deposit over the parent's remainder is refused by the vault", d.get("outcome") == "denied" and d.get("code") == "ANCESTOR_CAP", f"{d.get('code')} blocking grant {d.get('blocking_grant')}")
        r = b._spend(market, 50 * MILLI, "fits")
        check("P2 a deposit that fits the parent's remainder settles", (r.value or {}).get("outcome") == "settled")
        r = b._spend("relayer." + sb.root.account, 1 * MILLI, "not the market")
        d = r.value or {}
        check("P2 bidder key cannot pay anyone but the market", d.get("code") == "RECIPIENT_NOT_ALLOWED", d.get("code"))
        settle(rpc)

        # A root's contact record also counts everything its descendants paid, so a root must never
        # be credited from it: the market has to refuse root grants as bidders.
        cr = rpc.view(vault, "contact", {"grant_id": root.grant, "recipient": market})
        ca = rpc.view(vault, "contact", {"grant_id": a.grant, "recipient": market})
        check("P2 contact of a non-root grant is its own payments only", ca["total"] == str(60 * MILLI), f"A {int(ca['total']) / MILLI} mNEAR")
        check("P2 contact of the root aggregates the whole tree (so roots cannot be bidders)", cr and cr["total"] == str(110 * MILLI), f"root {int(cr['total']) / MILLI} mNEAR = A 60 + B 50, root itself paid 0")

        # The vault-issued key is bound to the vault by the protocol: it cannot call the market itself.
        try:
            r = near.Signer(rpc, vault, key_b).call(market, "credit_of", {"vault": vault, "grant_id": b.grant}, tgas=10)
            direct, how = False, f"went through: {r.status}"
        except near.RpcError as e:
            direct, how = "ReceiverMismatch" in json.dumps(e.data), "protocol: ReceiverMismatch"
        check("P3 the grant key cannot send a transaction to the market, so bids must be relayed and signed", direct, how)

        # P4: refund to the vault. Money comes back, the grant's counters do not move.
        info0 = rpc.view(vault, "vault_info")
        g0 = a.view()
        bal0 = w.balance(vault)
        r = relayer.call(market, "refund", {"vault": vault, "grant_id": a.grant, "amount": str(25 * MILLI)}, tgas=30)
        settle(rpc)
        info1 = rpc.view(vault, "vault_info")
        g1 = a.view()
        bal1 = w.balance(vault)
        check("P4 refund transfer to the vault succeeds", r.ok, r.failure())
        check("P4 vault balance and available_native rise by the refund", bal1 - bal0 == 25 * MILLI and int(info1["available_native"]) - int(info0["available_native"]) == 25 * MILLI, f"balance +{(bal1 - bal0) / MILLI} mNEAR")
        check("P4 grant counters do not move on a refund (budget is not restored)", g1["spent"] == g0["spent"] == str(60 * MILLI) and g1["spendable"] == g0["spendable"], f"spent {int(g1['spent']) / MILLI}, spendable {int(g1['spendable']) / MILLI}")
        c = rpc.view(vault, "contact", {"grant_id": a.grant, "recipient": market})
        check("P4 contact total is unchanged by a refund, so it is never credited again", c["total"] == str(60 * MILLI))

        # P6: revocation is visible to the probe at once, for the grant and for everything below it.
        d, _ = root.revoke(a.grant)
        check("revoke A by its parent", d.get("code") == "REVOKED", d.get("code"))
        r = ask(a.grant, key_a)
        v = r.value
        check("P6 probe sees a revoked grant as not live, in the next call", v["live"] is False and v["revoked"] is True)
        check("P6 credit of a revoked grant is still there to be returned", v["credit"] == str(35 * MILLI), f"{int(v['credit']) / MILLI} mNEAR")
        try:
            r = a._spend(market, 1 * MILLI, "after revoke")
            refused, how = (not r.ok) or (r.value or {}).get("outcome") == "denied", (r.value or {}).get("code") if r.ok else "transaction failed"
        except near.RpcError as e:
            refused, how = "NO_KEY" in str(e.data), "the vault deleted the key in the revoke, so the protocol has nothing to check"
        check("P6 revoked key cannot top up", refused, how)
        world.must(w.owner.call(vault, "revoke", {"grant_id": root.grant}, tgas=100), "revoke root")
        r = ask(b.grant, key_b)
        v = r.value
        # The eager sweep inside revoke also sets the child's own flag when the subtree is small, so
        # `live` is the field to read: it is false whether or not the sweep has reached the child.
        check("P6 revoking an ancestor makes the child not live", v["live"] is False, f"live {v['live']}, own revoked flag {v['revoked']}")

        # Clearance-bound grants: a deposit without a ticket goes to the approval queue, not to the market.
        root2 = w.root_agent(100 * MILLI, 100 * MILLI, clearance_required=True)
        r = root2._spend(market, 10 * MILLI, "no ticket")
        d = r.value or {}
        check("P5 on a clearance-bound grant a deposit with no ticket is queued, not paid", d.get("outcome") == "queued" and d.get("code") == "NEEDS_CLEARANCE", d.get("code"))

        subcontracting(w, rpc, vault, market, sb)

        summary = {
            "vault_wasm_sha256": vault_sha,
            "probe_wasm_sha256": hashlib.sha256(open(PROBE_WASM, "rb").read()).hexdigest(),
            "check_gas_tgas": round(gas_check / 1e12, 2),
            "passed": sum(1 for x in RESULTS if x["pass"]),
            "total": len(RESULTS),
            "results": RESULTS,
        }
        json.dump(summary, open(OUT, "w"), indent=2)
        print(f"{summary['passed']}/{summary['total']} checks passed", flush=True)
        w.close()
    return 0 if all(x["pass"] for x in RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
