#!/usr/bin/env python3
"""Maps what the two official reference products say about their own limits to a test here, and takes
each result from the evidence reports. Writes docs/BASELINE-GAPS.md.

Sources are the public documentation pages only, read on 2026-11-05. No production service and no
demo environment was touched: nothing here tests their products. It tests this contract against the
same situation, so "result" always means a result on Espalier."""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
E = os.path.join(HERE, "..", "docs", "evidence")
API = "https://docs.agentsonintents.com/integration/agent-api/policies-and-limits"
OUTLAYER = "https://app.outlayer.ai/docs/agent-custody"

ROWS = [
    ("Agent API", "Allowed actions, `native`", "Native methods sit outside the policy: destinations, USD budget, timelock, schedule and owner approval do not stop them. With `native.near.transfer` on, the agent can send NEAR to any account.",
     "There is no way to move funds around the policy. The agent's key cannot transfer at all, and the only paths to the money are `request_spend` and `execute_approved`.", "B08 H15", "Closed"),
    ("Agent API", "USD budget, periods", "Periods are fixed. Around a reset an agent can spend up to twice the cap within minutes. The page offers smaller caps or timelocks as a way to bound it.",
     "The window is rolling, with no boundary to straddle. A payment split to fit is still counted.", "E08 E11", "Closed"),
    ("Agent API", "USD budget, when a cap counts", "A cap counts only while it is on. Turning it off and on again starts a new period, and earlier spending no longer counts.",
     "Terms cannot change after creation: no method writes them. A wider budget means a new grant from the owner or an ancestor, and it still counts against every ancestor.", "A16 A17", "Closed for child grants. The owner can always create a new root"),
    ("Agent API", "USD budget, prices", "The budget needs a price less than 10 minutes old, otherwise a counted execution is refused.",
     "No equivalent. Limits are in units of one asset, so no price is involved, and there is no budget across assets either.", "", "Not comparable: this project has no USD budget"),
    ("Agent API", "USD budget, when the charge happens", "The charge is decided at dispatch in one database transaction, so concurrent requests cannot exceed a cap. An uncertain outcome stays charged.",
     "The same guarantee, from one receipt instead of one database transaction. A transfer that fails is known from the callback, so it is released.", "E06 E03 E04", "Not a documented gap: this product is exact here too"),
    ("OutLayer", "Velocity limits are best-effort under concurrency", "Daily, hourly and monthly caps and the rate limit are checked against a counter the gateway supplies, and the TEE keeps no state, so concurrent requests can read the same counter and exceed the cap by the batch in flight.",
     "Counters live in contract state and a reservation at every level is made in the same receipt as the check. Receipts on one account run one at a time.", "E06 E07 H11", "Closed"),
    ("OutLayer", "How it works", "The gateway keeps operational state: usage and pending approvals.",
     "Usage, reservations and the approval queue are contract state, readable by anyone with a view call.", "D01 E01", "Closed. The cost is gas and storage on every spend"),
    ("OutLayer", "Multisig also covers NEAR Intents operations", "For Intents operations the TEE does not re-check token or amount, and the gateway builds the deposit address at execution. The page says a compromised gateway could substitute token, amount or routing after approval.",
     "An approval commits to a hash of vault, grant, asset, recipient, amount, memo and nonce, and the arguments are hashed again at execution. **This covers direct payments only: Espalier has no Intents path**, so the Intents case itself is not tested.", "D03 D04 D02", "Closed for direct payments, not applicable to Intents"),
    ("OutLayer", "Audit log", "`GET /wallet/v1/audit` returns history. The page does not describe how a reader can tell it was not altered.",
     "Every decision is a NEP-297 event in a receipt on chain, with reason code and numbers. `scripts/verify-testnet.py` reads them back with no key.", "G01", "Closed for what happens on chain"),
]

THEY_HAVE = [
    "Keys held in a TEE, with attestation. Here the agent holds its own function-call key, and a stolen key is bounded by the grant, not prevented.",
    "Cross-chain payments through NEAR Intents, and swaps. Here: NEAR and NEP-141 tokens on NEAR, on testnet.",
    "USD budgets across assets.",
    "Time windows, schedules and timelocks.",
    "Multi-approver multisig. Here: the owner, or one ancestor within its threshold.",
    "A hosted product with a dashboard, in production. This is a contract and a client on testnet, not audited.",
]


def load(name):
    p = os.path.join(E, name)
    return json.load(open(p, encoding="utf8")) if os.path.exists(p) else {"scenarios": []}


def main():
    sandbox = {s["id"]: s for s in load("sandbox-report.json")["scenarios"]}
    testnet = {s["id"]: s for s in load("testnet-run.json")["scenarios"]}
    out = [
        "# Documented gaps in the official reference products",
        "",
        "Generated by `scripts/baseline.py`. Do not edit by hand.",
        "",
        "The security track points to two products as starting points. Their documentation is candid about its limits, which makes a good test list. **Everything below comes from the public documentation pages, read on 2026-11-05. No production service or demo environment was touched, and nothing here is a finding about those products beyond what their own pages say.** The Result column is a result on Espalier.",
        "",
        f"- Agent API: {API}",
        f"- OutLayer Agent Custody: {OUTLAYER}",
        "",
        "| Product | Section | What the page says | What Espalier does | Tests | Sandbox | Testnet | Status |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for product, section, says, ours, ids, status in ROWS:
        ids = ids.split()
        sb = ", ".join(f"{i} {sandbox[i]['status']}" for i in ids) or "-"
        tn = ", ".join(f"{i} {testnet[i]['status']}" for i in ids if i in testnet) or "-"
        tests = ", ".join(ids) or "-"
        out.append(f"| {product} | {section} | {says} | {ours} | {tests} | {sb} | {tn} | {status} |")
    out += [
        "",
        "A correction to an earlier note of ours: concurrency counting is a documented limit of OutLayer, not of the Agent API, whose page says concurrent requests cannot exceed a cap.",
        "",
        "## What those products have and this one does not",
        "",
    ]
    out += [f"- {x}" for x in THEY_HAVE]
    out += ["", "Scenario details: [ATTACKS.md](ATTACKS.md). Testnet transactions: [evidence/testnet-run-txs.md](evidence/testnet-run-txs.md).", ""]
    with open(os.path.join(HERE, "..", "docs", "BASELINE-GAPS.md"), "w", encoding="utf8", newline="\n") as f:
        f.write("\n".join(out))
    print(f"{len(ROWS)} rows")


main()
