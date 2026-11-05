#!/usr/bin/env python3
"""Merges the per-process reports of scripts/property.sh into one evidence file."""
import collections, glob, json, os, sys

reports = [json.load(open(p)) for p in sorted(glob.glob(os.path.join(sys.argv[1], "*.json")))]
ops, codes = collections.Counter(), collections.Counter()
for r in reports:
    ops.update(r["operations"])
    codes.update(r["denial_codes"])
doc = {
    "harness": reports[0]["harness"],
    "processes": len(reports),
    "seeds": sum(r["seeds"] for r in reports),
    "ops_per_seed": reports[0]["ops_per_seed"],
    "total_ops": sum(r["total_ops"] for r in reports),
    "invariant_sweeps": sum(r["invariant_sweeps"] for r in reports),
    "longest_process_seconds": max(r["seconds"] for r in reports),
    "max_tree_depth_seen": max(r["max_tree_depth_seen"] for r in reports),
    "max_grants_in_one_run": max(r["max_grants_in_one_run"] for r in reports),
    "window_refusals_within_bucket_slack": sum(r["window_refusals_within_bucket_slack"] for r in reports),
    "operations": dict(sorted(ops.items())),
    "denial_codes": dict(sorted(codes.items())),
    "invariants": reports[0]["invariants"],
}
json.dump(doc, open(sys.argv[2], "w"), indent=2)
print(json.dumps({k: doc[k] for k in ("total_ops", "invariant_sweeps", "max_tree_depth_seen", "max_grants_in_one_run")}))
print(json.dumps(doc["denial_codes"]))
print(json.dumps(doc["operations"]))
