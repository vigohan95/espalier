#!/usr/bin/env python3
"""Replays one recorded trial from the stored files: the document, the model's raw answer, the
recipient check decided again from the stored signals, and what the contract did. Offline, no model.

    python3 trials/replay.py I01
    python3 trials/replay.py --all     # every trial, one line each, and a count of verdict mismatches
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
E = os.path.join(HERE, "..", "docs", "evidence", "trials")
sys.path.insert(0, os.path.join(HERE, "..", "client"))

from espalier import policy  # noqa: E402


def table(name):
    return {r["id"]: r for r in (json.loads(l) for l in open(os.path.join(E, name), encoding="utf8"))}


def decide_again(r, pol):
    if not r.get("signals"):
        return None
    return policy.decide(r["signals"], int(r["amount"]), pol)


def show(i, corpus, transcripts, results, pol):
    c, t, r = corpus[i], transcripts[i], results[i]
    print(f"trial {i}: {c['kind']}, {c['subkind']}")
    print(f"the true payee is {c['truth_recipient']}, {c['truth_amount']} NEAR\n")
    print("--- document the agent read ---")
    print(c["document"].rstrip())
    print(f"\n--- {t['model']} answered, temperature {t['temperature']}, seed {t['seed']} ---")
    print(t["raw_answer"].strip())
    print(f"\nmodel outcome: {r['model_outcome']}")
    d = decide_again(r, pol)
    if d is None:
        print("no spend reached the check")
        return
    same = d["verdict"] == r["verdict"]
    print(f"\n--- recipient check, decided again now from the stored signals: {d['verdict']} (recorded: {r['verdict']}, same: {same}) ---")
    for x in d["reasons"]:
        print(f"  {x['level']:5} {x['code']}: {x['detail']}")
    print("\n--- what happened on the node ---")
    print(f"  wrapper signed a transaction: {r.get('signed_a_transaction')}")
    print(f"  ended as: {r.get('guardrail')} {r.get('contract_code', '')}")
    print(f"  grant spent in this trial: {int(r['spent_delta_yocto']) / 10**24} NEAR")
    for h in r.get("txs", []):
        print(f"  tx {h} (local node)")


def main():
    corpus, transcripts, results = table("corpus.jsonl"), table("transcripts.jsonl"), table("results.jsonl")
    meta = json.load(open(os.path.join(E, "run-meta.json")))
    pol = policy.Policy(**meta["policy"])
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    if sys.argv[1] == "--all":
        bad = 0
        for i in sorted(results, key=lambda x: (x[0], int(x[1:]))):
            r = results[i]
            d = decide_again(r, pol)
            ok = d is None or d["verdict"] == r["verdict"]
            bad += not ok
            print(i, r["model_outcome"], r.get("guardrail", "-"), r.get("verdict", "-"), "" if ok else "MISMATCH")
        print(f"{bad} mismatches")
        sys.exit(1 if bad else 0)
    show(sys.argv[1], corpus, transcripts, results, pol)


main()
