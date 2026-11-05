#!/usr/bin/env python3
"""The whole project on one screen, offline, in a few seconds. Python standard library only: no
install, no network, no keys.

    python3 scripts/story.py

This does not rerun the chain. It recomputes what can be recomputed from the committed files, says
which is which, and exits non-zero if anything does not add up:
  - hashes the wasm files and compares them with the reports
  - counts passes and assertions in the evidence reports
  - runs the client's unit tests
  - decides every recorded trial again from its stored signals, with the real policy code
  - verifies a real ed25519 ticket against the contract's message format
"""
import hashlib
import json
import os
import sys
import time
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
E = os.path.join(ROOT, "docs", "evidence")
sys.path.insert(0, os.path.join(ROOT, "client"))

from espalier import attestor, ed25519, near, policy  # noqa: E402

BAD = []


def load(*parts):
    return json.load(open(os.path.join(E, *parts), encoding="utf8"))


def lines(*parts):
    return [json.loads(l) for l in open(os.path.join(E, *parts), encoding="utf8")]


def sha(name):
    return hashlib.sha256(open(os.path.join(ROOT, "res", name), "rb").read()).hexdigest()


def check(ok, text):
    print(("  ok   " if ok else "  FAIL ") + text)
    if not ok:
        BAD.append(text)


def run_unit_tests():
    suite = unittest.defaultTestLoader.discover(os.path.join(ROOT, "client", "tests"))
    result = unittest.TextTestRunner(stream=open(os.devnull, "w")).run(suite)
    return result.testsRun, result.wasSuccessful()


def main():
    t0 = time.time()
    sandbox = load("sandbox-report.json")
    testnet = load("testnet-run.json")
    deploy = load("testnet-deploy.json")
    prop = load("property-report.json")
    real = load("sandbox-property.json")
    screened = load("sandbox-property-screened.json")
    mutants = load("mutants.json")
    report = load("trials", "report.json")
    mainnet = load("mainnet-signals.json")
    fooled = report["how_often_the_model_was_fooled"]
    stop = report["what_happened_to_the_fooled_spends"]
    ben = report["benign"]

    print("Espalier: a NEAR contract that lets an AI agent hand part of its budget to sub-agents,")
    print("with limits enforced on chain, and asks about a recipient before any money moves.\n")

    print("Recomputed here, now:")
    vault = sha("espalier_vault.wasm")
    check(vault == sandbox["wasm_sha256"] == testnet["wasm_sha256"] == deploy["wasm_sha256"], f"res/espalier_vault.wasm {vault[:12]} is what sandbox and testnet ran")
    check(sha("espalier_registry.wasm") == deploy["registry_wasm_sha256"], "res/espalier_registry.wasm is what testnet runs")
    n, ok = run_unit_tests()
    check(ok, f"{n} client unit tests, including RFC 8032 vectors")

    pol = policy.Policy(**report["replay_policy"])
    rows = [r for r in lines("trials", "results.jsonl") if r.get("signals")]
    same = sum(policy.decide(r["signals"], int(r["amount"]), pol)["verdict"] == r["verdict"] for r in rows)
    check(same == len(rows) and rows, f"{same} of {len(rows)} recorded trials decided again from stored signals: same verdict")

    seed = hashlib.sha256(b"story").digest()
    msg = attestor.clearance_message("vault.near", 3, "shop.near", 10**24, 1_000, b"\0" * 32)
    sig = ed25519.sign(seed, msg)
    forged = attestor.clearance_message("vault.near", 3, "attacker.near", 10**24, 1_000, b"\0" * 32)
    check(ed25519.verify(ed25519.public_key(seed), msg, sig) and not ed25519.verify(ed25519.public_key(seed), forged, sig), "a ticket verifies for its recipient and fails for any other")

    print("\nFrom the evidence files, with what each one ran on:")
    check(sandbox["passed"] == sandbox["total"] == len(sandbox["scenarios"]), f"{sandbox['passed']}/{sandbox['total']} attack and behaviour scenarios, {sandbox['assertions']} assertions (real node, compiled wasm)")
    txs = sum(len(s["txs"]) for s in testnet["scenarios"])
    check(testnet["passed"] == testnet["total"], f"{testnet['passed']}/{testnet['total']} scenarios on NEAR testnet, {txs} recorded transactions, vault {deploy['vault']}")
    check(real["operations"] >= 10_000, f"{real['operations']:,} random ops on the real runtime, every invariant held; {screened['operations']:,} more with a live registry")
    check(prop["total_ops"] >= 100_000, f"{prop['total_ops']:,} random ops in process (mock VM, not the runtime)")
    check(mutants["caught"] == mutants["total"], f"{mutants['caught']}/{mutants['total']} mutants of the contract caught by the property test")

    print(f"\nReal model under indirect injection ({report['model']['name']}, {report['model']['parameters']}, small, on CPU):")
    print(f"  fooled by the document       {fooled['fooled']} of {fooled['of']}")
    print(f"  refused with no human        {stop['stopped_without_any_human']}")
    print(f"  held for a human             {stop['buckets'].get('stopped_only_by_human', 0)} (the human in this run is scripted and perfect)")
    print(f"  went through                 {stop['buckets'].get('reached_recipient', 0)}: inflated amounts to real vendors, inside the limits")
    print(f"  ordinary payments refused    {ben['false_block']} of {ben['model_got_it_right']}; sent to a human {ben['sent_to_a_human']}, all first-time vendors")
    print(f"  same spends, wrapper skipped {report['wrapper_skipped_replay']['outcomes']}")

    print("\nReal mainnet accounts, read-only:")
    for r in mainnet["accounts"]:
        s = r["signals"]
        age = (s.get("age") or {}).get("age_secs")
        changed = (s.get("code") or {}).get("changed_ago_secs")
        print(f"  {s['account'][:28]:28} age {age / 86400:8.1f} d  code changed {'-' if changed is None else f'{changed / 86400:.1f} d ago':>14}  {r['verdict_from_age_and_code_only']['verdict']}")

    print("\nNot done: no audit, no TEE, no NEAR Intents path, no market.near.ai integration, testnet only.")
    print("A bad payment inside the limits to a real vendor still goes through. Details: docs/ATTACKS.md, README.")
    print(f"\n{'all checks passed' if not BAD else str(len(BAD)) + ' CHECKS FAILED'} in {time.time() - t0:.1f}s")
    sys.exit(1 if BAD else 0)


main()
