"""Offline tests for the client. Run: python3 -m unittest discover -s client/tests"""
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from espalier import attestor, ed25519, near, policy  # noqa: E402

DAY = 86_400


def clean_signals(**over):
    s = {
        "account": "vendor.near",
        "exists": True,
        "is_contract": False,
        "age": {"age_secs": 400 * DAY},
        "denylist": {"listed": False, "trusted_reports": 0, "distinct_evidence": 0, "all_reports": 0, "responded": False},
        "history": {"agent": {"count": 5, "total": "500", "first_ms": 0, "last_ms": 2 * DAY * 1000}, "tree": None},
    }
    s.update(over)
    return s


def codes(result):
    return {r["code"] for r in result["reasons"]}


class Ed25519(unittest.TestCase):
    # RFC 8032 section 7.1, tests 1 and 3.
    VECTORS = [
        ("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
         "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "",
         "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
        ("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
         "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025", "af82",
         "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a"),
    ]

    def test_vectors(self):
        for seed, pub, msg, sig in self.VECTORS:
            seed, pub, msg, sig = map(bytes.fromhex, (seed, pub, msg, sig))
            self.assertEqual(ed25519.public_key(seed), pub)
            self.assertEqual(ed25519.sign(seed, msg), sig)
            self.assertTrue(ed25519.verify(pub, msg, sig))
            self.assertFalse(ed25519.verify(pub, msg + b"x", sig))


class Encoding(unittest.TestCase):
    def test_base58_round_trip(self):
        for raw in (b"\0" * 32, b"\0\0abc", bytes(range(32))):
            self.assertEqual(near.b58decode(near.b58encode(raw)), raw)
        self.assertEqual(near.b58encode(b"\0" * 32), "1" * 32)

    def test_key_string_round_trip(self):
        k = near.Key(b"\x01" * 32)
        self.assertEqual(near.Key.from_string(k.secret_str).public, k.public)

    def test_clearance_message_layout(self):
        m = attestor.clearance_message("v.near", 7, "r.near", 5, 9, b"\x02" * 32)
        want = (b"espalier-clearance-v1" + b"\x06\0\0\0v.near" + (7).to_bytes(8, "little")
                + b"\x06\0\0\0r.near" + (5).to_bytes(16, "little") + (9).to_bytes(8, "little") + b"\x02" * 32)
        self.assertEqual(m, want)


class Decide(unittest.TestCase):
    P = policy.Policy(known_names=["acme-billing.near"])

    def test_known_clean_recipient_is_allowed(self):
        r = policy.decide(clean_signals(), 100, self.P)
        self.assertEqual(r["verdict"], "allow")
        self.assertIn("KNOWN_RECIPIENT", codes(r))

    def test_missing_account_blocks(self):
        self.assertEqual(policy.decide({"account": "x.near", "exists": False}, 1, self.P)["verdict"], "block")

    def test_quorum_blocks(self):
        d = {"listed": True, "trusted_reports": 2, "distinct_evidence": 2, "all_reports": 2, "responded": True}
        r = policy.decide(clean_signals(denylist=d), 100, self.P)
        self.assertEqual(r["verdict"], "block")
        self.assertEqual(codes(r) >= {"DENYLIST_QUORUM", "SUBJECT_RESPONDED"}, True)

    def test_one_trusted_report_only_warns(self):
        d = {"listed": False, "trusted_reports": 1, "distinct_evidence": 1, "all_reports": 1, "responded": False}
        r = policy.decide(clean_signals(denylist=d), 100, self.P)
        self.assertEqual((r["verdict"], "DENYLIST_REPORTED" in codes(r)), ("warn", True))

    def test_untrusted_reports_change_nothing(self):
        d = {"listed": False, "trusted_reports": 0, "distinct_evidence": 0, "all_reports": 50, "responded": False}
        r = policy.decide(clean_signals(denylist=d), 100, self.P)
        self.assertEqual(r["verdict"], "allow")
        self.assertIn("UNTRUSTED_REPORTS", codes(r))

    def test_unreadable_signals_warn_never_allow(self):
        for over in ({"denylist": None}, {"age": None}, {"is_contract": True, "code": None}):
            self.assertEqual(policy.decide(clean_signals(**over), 100, self.P)["verdict"], "warn", over)

    def test_young_account_warns(self):
        r = policy.decide(clean_signals(age={"age_secs": 3600}), 100, self.P)
        self.assertIn("ACCOUNT_YOUNG", codes(r))

    def test_recent_code_change_warns(self):
        code = {"changed_ago_secs": 3600, "changed_block": 5}
        r = policy.decide(clean_signals(is_contract=True, code=code, keys={"full_access_keys": 1}), 100, self.P)
        self.assertEqual(r["verdict"], "warn")
        self.assertEqual(codes(r) >= {"CODE_CHANGED_RECENTLY", "CONTRACT_UPGRADEABLE"}, True)

    def test_global_contract_counts_as_a_contract(self):
        from espalier import signals
        by_hash = {"code_hash": signals.ZERO_HASH, "global_contract_hash": "Abc"}
        by_account = {"code_hash": signals.ZERO_HASH, "global_contract_account_id": "pub.near"}
        self.assertEqual(signals.code_of(by_hash), "Abc")
        self.assertEqual(signals.code_of(by_account), "global:pub.near")
        self.assertEqual(signals.code_of({"code_hash": "Xyz"}), "Xyz")

    def test_code_that_follows_a_publisher_warns(self):
        s = clean_signals(is_contract=True, code={"changed_ago_secs": None}, code_hash="global:pub.near", code_follows_publisher=True)
        self.assertIn("CODE_FOLLOWS_PUBLISHER", codes(policy.decide(s, 100, self.P)))

    def test_new_recipient_warns_and_near_miss_is_named(self):
        s = clean_signals(account="acme-bi11ing.near", history={"agent": None, "tree": None})
        self.assertEqual(codes(policy.decide(s, 100, self.P)) >= {"NO_PRIOR_PAYMENT", "NAME_NEAR_MISS"}, True)

    def test_dust_burst_does_not_make_a_recipient_known(self):
        burst = {"count": 40, "total": "40", "first_ms": 0, "last_ms": 5000}
        s = clean_signals(history={"agent": burst, "tree": None})
        self.assertIn("LITTLE_PRIOR_PAYMENT", codes(policy.decide(s, 1, self.P)))

    def test_amount_jump_warns(self):
        r = policy.decide(clean_signals(), 500, self.P)
        self.assertEqual((r["verdict"], "AMOUNT_JUMP" in codes(r)), ("warn", True))

    def test_tree_history_counts_for_a_new_worker(self):
        s = clean_signals(history={"agent": None, "tree": {"count": 5, "total": "500", "first_ms": 0, "last_ms": 2 * DAY * 1000}})
        self.assertEqual(policy.decide(s, 100, self.P)["verdict"], "allow")


if __name__ == "__main__":
    unittest.main()
