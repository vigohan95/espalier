"""Offline tests for the market client and the market model.
Run: python3 -m unittest discover -s client/tests"""
import hashlib
import json
import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from espalier import market, market_model, near  # noqa: E402

ROOT = os.path.join(os.path.dirname(__file__), "..", "..")
MILLI = near.MILLI

SPEC = {
    "vault": "vault.test.near", "grant_id": 7, "spec_hash": "ab" * 32, "max_budget": "100", "bid_bond": "5", "min_bids": 2,
    "commit_end_ms": 1000, "reveal_end_ms": 2000, "award_end_ms": 3000, "deliver_end_ms": 4000, "review_end_ms": 5000,
    "sub_depth": 1, "sub_nodes": 3, "sub_gas": "9", "key_allowance": "11", "nonce": 42,
}
# sha256 of the post message for SPEC on market.test.near. The same constant is asserted by the
# contract's own unit test, so the two byte layouts cannot drift apart.
POST_SHA256 = "08a724c89a95c3591d5e913e127f254566f6dc42685e98ce21d17778f5a0729b"


class Layout(unittest.TestCase):
    def test_post_message_is_tag_then_borsh_of_market_and_spec(self):
        def s(x):
            return struct.pack("<I", len(x)) + x.encode()

        want = (
            b"espalier-market-post-v1" + s("market.test.near") + s("vault.test.near") + struct.pack("<Q", 7) + s("ab" * 32)
            + (100).to_bytes(16, "little") + (5).to_bytes(16, "little") + bytes([2])
            + b"".join(struct.pack("<Q", t) for t in (1000, 2000, 3000, 4000, 5000))
            + bytes([1]) + struct.pack("<I", 3) + (9).to_bytes(16, "little") + (11).to_bytes(16, "little") + struct.pack("<Q", 42)
        )
        self.assertEqual(market.post_message("market.test.near", SPEC), want)

    def test_post_message_hash_matches_the_contract(self):
        self.assertEqual(hashlib.sha256(market.post_message("market.test.near", SPEC)).hexdigest(), POST_SHA256)

    def test_review_messages_are_bound_to_market_task_and_reason(self):
        a = market.accept_message("m.near", 1)
        self.assertNotEqual(a, market.accept_message("m.near", 2))
        self.assertNotEqual(a, market.accept_message("n.near", 1))
        self.assertNotEqual(a, market.reject_message("m.near", 1, ""))
        self.assertNotEqual(market.reject_message("m.near", 1, "aa"), market.reject_message("m.near", 1, "ab"))

    def test_commitment_names_the_worker_the_task_and_the_key(self):
        salt = b"\1" * 32
        c = market.bid_commitment("m.near", 1, "w.near", 10, "ed25519:abc", salt)
        self.assertEqual(len(c), 64)
        for other in (
            market.bid_commitment("m.near", 2, "w.near", 10, "ed25519:abc", salt),
            market.bid_commitment("m.near", 1, "x.near", 10, "ed25519:abc", salt),
            market.bid_commitment("m.near", 1, "w.near", 11, "ed25519:abc", salt),
            market.bid_commitment("m.near", 1, "w.near", 10, "ed25519:abd", salt),
            market.bid_commitment("m.near", 1, "w.near", 10, "ed25519:abc", b"\2" * 32),
            market.bid_commitment("n.near", 1, "w.near", 10, "ed25519:abc", salt),
        ):
            self.assertNotEqual(c, other)


class Pricing(unittest.TestCase):
    def test_shared_vectors(self):
        with open(os.path.join(ROOT, "market", "fixtures", "pricing-vectors.json"), encoding="utf8") as f:
            doc = json.load(f)
        self.assertGreaterEqual(len(doc["cases"]), 10)
        for c in doc["cases"]:
            got = market.pick([(int(p), s) for p, s in c["bids"]], int(c["max_budget"]), c["min_bids"])
            want = None if c["winner"] is None else (c["winner"], int(c["price"]))
            self.assertEqual(got, want, c["name"])


def world():
    m = market_model.Model(poster_bond=10)
    m.set_roster("owner", ["a", "b", "c"])
    root = m.create_root("k0", terms(m, 300, depth=3, nodes=20))
    return m, root


def terms(m, cap, depth=0, nodes=0, **over):
    t = {
        "cap": cap, "per_tx": cap, "window_cap": cap, "approval_threshold": cap, "recipients": [m.market],
        "methods": ["request_spend", "delegate", "revoke"], "expires_at_ms": 10**9, "max_depth": depth, "max_denials": 20,
        "key_allowance": 1, "gas_budget": 100 * nodes, "node_budget": nodes, "clearance_required": False,
    }
    t.update(over)
    return t


def spec(m, grant, budget, **over):
    s = {
        "vault": m.vault, "grant_id": grant, "spec_hash": "00" * 32, "max_budget": budget, "bid_bond": 5, "min_bids": 1,
        "commit_end_ms": m.now + 1000, "reveal_end_ms": m.now + 2000, "award_end_ms": m.now + 3000, "deliver_end_ms": m.now + 4000,
        "review_end_ms": m.now + 5000, "sub_depth": 0, "sub_nodes": 0, "sub_gas": 0, "key_allowance": 1, "nonce": len(m.tasks) + 1,
    }
    s.update(over)
    return s


def hire(m, poster, key, budget, bids, **over):
    """Post, bid, reveal, close, award. `bids` maps worker -> (price, key). Returns (task, grant, price)."""
    code, tid = m.post(spec(m, poster, budget, **over), key, 10, "carrier")
    assert code == "POSTED", code
    t = m.tasks[tid]
    for w, (price, k) in bids.items():
        assert m.commit(w, tid, (price, k, b"s"), 5) == "ok"
    m.now = t["spec"]["commit_end_ms"]
    for w, (price, k) in bids.items():
        assert m.reveal(w, tid, price, k, b"s") == "REVEALED"
    m.now = t["spec"]["reveal_end_ms"]
    code, price = m.close(tid)
    assert code == "WINNER", code
    winner = t["bids"][t["winner"]]
    depth = t["spec"]["sub_depth"]
    code, gid = m.delegate(poster, winner["pk"], terms(m, price, depth=depth, nodes=t["spec"]["sub_nodes"], gas_budget=t["spec"]["sub_gas"]))
    assert code == "GRANTED", code
    assert m.confirm(tid, gid) == "AWARDED"
    return tid, gid, price


class Rules(unittest.TestCase):
    def test_second_price_and_exact_charge(self):
        m, root = world()
        tid, gid, price = hire(m, root, "k0", 100, {"a": (40, "ka"), "b": (55, "kb"), "c": (70, "kc")})
        self.assertEqual(price, 55)
        self.assertEqual(m.deliver("a", tid), "ok")
        self.assertEqual(m.review(tid, True, "ka"), "BAD_SIGNATURE")
        self.assertEqual(m.review(tid, True, "k0"), "ACCEPTED")
        self.assertEqual(m.spend(gid, m.market, 56), ("PER_TX", gid))
        self.assertEqual(m.spend(gid, m.market, 20), ("SETTLED", None))
        self.assertEqual(m.spend(gid, m.market, 40), ("CAP", gid))
        self.assertEqual(m.spend(gid, m.market, 35), ("SETTLED", None))
        self.assertEqual(m.claim(tid), ("CLAIMED", 55))
        self.assertEqual(m.grants[root]["spent"], 55)
        self.assertEqual(m.tasks[tid]["status"], "Paid")

    def test_no_pay_before_acceptance_and_refund_goes_to_the_vault(self):
        m, root = world()
        tid, gid, price = hire(m, root, "k0", 100, {"a": (40, "ka")})
        self.assertEqual(price, 100)
        m.spend(gid, m.market, 30)
        self.assertEqual(m.claim(tid), ("CLAIMED", 0))
        m.deliver("a", tid)
        self.assertEqual(m.review(tid, False, "k0"), "REJECTED")
        self.assertEqual(m.refund(tid), "REFUNDED")
        self.assertEqual(m.balances[m.vault], 30)

    def test_promise_is_bounded_and_draws_stop_at_the_ancestor(self):
        m, root = world()
        t1, g1, _ = hire(m, root, "k0", 100, {"a": (50, "ka")}, sub_depth=1, sub_nodes=4, sub_gas=40)
        code, _ = m.post(spec(m, g1, 101), "ka", 10, "a")
        self.assertEqual(code, "LIMITS_SHORT")
        t2, g2, _ = hire(m, g1, "ka", 60, {"b": (30, "kb")})
        code, _ = m.post(spec(m, g1, 41), "ka", 10, "a")
        self.assertEqual(code, "OVERCOMMITTED")
        self.assertEqual(m.spend(g1, m.market, 70), ("SETTLED", None))
        self.assertEqual(m.spend(g2, m.market, 60), ("ANCESTOR_CAP", g1))
        self.assertEqual(m.spend(g2, m.market, 30), ("SETTLED", None))
        self.assertEqual(m.grants[g1]["spent"], 100)
        self.assertEqual(m.grants[root]["spent"], 100)

    def test_revoke_cuts_the_branch_and_voids_cost_no_bond(self):
        m, root = world()
        t1, g1, _ = hire(m, root, "k0", 100, {"a": (50, "ka")}, sub_depth=1, sub_nodes=4, sub_gas=40)
        t2, g2, _ = hire(m, g1, "ka", 40, {"b": (30, "kb")})
        self.assertEqual(m.void(t2), "STILL_LIVE")
        self.assertEqual(m.revoke("owner", g1), "REVOKED")
        self.assertFalse(m.live(g2))
        self.assertEqual(m.spend(g2, m.market, 1), ("NO_KEY", None))
        self.assertEqual(m.void(t2), "VOID")
        self.assertEqual(m.void(t1), "VOID")
        self.assertEqual(m.balances["b"], 5)
        self.assertEqual(m.balances.get(m.vault, 0), 0)

    def test_default_pays_the_winner_the_poster_bond(self):
        m, root = world()
        code, tid = m.post(spec(m, root, 50), "k0", 10, "carrier")
        m.commit("a", tid, (20, "ka", b"s"), 5)
        m.now += 1000
        m.reveal("a", tid, 20, "ka", b"s")
        m.now += 1000
        m.close(tid)
        self.assertEqual(m.default(tid), "WRONG_PHASE")
        m.now += 1000
        self.assertEqual(m.default(tid), "DEFAULTED")
        self.assertEqual(m.balances["a"], 15)
        self.assertEqual(m.posters[root]["defaults"], 1)


if __name__ == "__main__":
    unittest.main()
