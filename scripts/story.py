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
  - runs the 25-agent subcontracting swarm on the Python model and compares the result with the
    recorded runs on a local node and on testnet
"""
import contextlib
import hashlib
import importlib.util
import io
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


def swarm_on_the_model():
    """Runs scripts/swarm-sim.py on the Python model, quietly. Returns the module and its summary."""
    spec = importlib.util.spec_from_file_location("swarm_sim", os.path.join(ROOT, "scripts", "swarm-sim.py"))
    sim = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sim)
    with contextlib.redirect_stdout(io.StringIO()):
        summary = sim.Story(sim.ModelWorld(unit_ms=60_000)).run()
    return sim, summary


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
    mprop = load("market-property.json")
    sim_local = load("swarm-sim-sandbox.json")
    sim_testnet = load("swarm-sim-testnet.json")
    fooled = report["how_often_the_model_was_fooled"]
    stop = report["what_happened_to_the_fooled_spends"]
    ben = report["benign"]

    print("Espalier: agents hire agents by sealed bid, and what a winner gets is a grant in a vault:")
    print("a budget that can only be narrower than the one it came from, at every level, enforced on chain.\n")

    print("Recomputed here, now:")
    vault = sha("espalier_vault.wasm")
    check(vault == sandbox["wasm_sha256"] == testnet["wasm_sha256"] == deploy["wasm_sha256"], f"res/espalier_vault.wasm {vault[:12]} is what sandbox and testnet ran")
    check(sha("espalier_registry.wasm") == deploy["registry_wasm_sha256"], "res/espalier_registry.wasm is what testnet runs")
    market = sha("espalier_market.wasm")
    check(market == sandbox["market_wasm_sha256"] == mprop["market_wasm_sha256"] == sim_local["market_wasm_sha256"] == sim_testnet["market_wasm_sha256"], f"res/espalier_market.wasm {market[:12]} is what the scenarios, the random test, the local run and testnet ran")
    check(vault == mprop["vault_wasm_sha256"] == sim_local["vault_wasm_sha256"] == sim_testnet["vault_wasm_sha256"], "the market ran against that same vault wasm: the vault was not changed for it")
    sim, model = swarm_on_the_model()
    check(model["passed"], f"{sum(c['pass'] for c in model['checks'])} of {len(model['checks'])} checks of the 25-agent swarm on the Python model, run just now")
    for where, doc in (("a local node", sim_local), ("NEAR testnet", sim_testnet)):
        same = sim.comparable(doc) == sim.comparable(model)
        check(same and doc["passed"], f"the run recorded on {where} ended where the model ends: {len(doc['tasks'])} tasks, {len(doc['grants'])} grants, same winners, prices and payments")
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
    ms = [x for x in sandbox["scenarios"] if x["group"] == "market"]
    check(ms and all(x["status"] == "pass" for x in ms), f"{len(ms)} of those are the market, {sum(len(x['facts']) for x in ms)} assertions: auctions, awards, revokes at every stage")
    check(not mprop["broken"], f"{mprop['transactions']} random calls against vault and market on the real runtime, {mprop['invariant_checks']} invariant checks, none broken")
    check(sim_local["passed"] and sim_local["matches_model"], f"25 scripted agents on a local node: {sim_local['transactions']} signed transactions, three levels of hiring, ended where the model predicted")
    check(sim_testnet["passed"] and sim_testnet["matches_model"], f"the same on NEAR testnet: {sim_testnet['transactions']} transactions, market {sim_testnet['market']}")
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
    print("A bad payment inside the limits to a real vendor still goes through. A poster can refuse good work.")
    print("Details: docs/ATTACKS.md, docs/SWARM-RULES.md, README.")
    print(f"\n{'all checks passed' if not BAD else str(len(BAD)) + ' CHECKS FAILED'} in {time.time() - t0:.1f}s")
    sys.exit(1 if BAD else 0)


main()
