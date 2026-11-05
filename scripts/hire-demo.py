#!/usr/bin/env python3
"""A general contractor agent with a budget. It splits a task, buys one part itself, hires workers
for the others, counters a high quote once, checks the work, pays, and writes a receipt per task.

    NEAR_SANDBOX_BIN_PATH=/path/to/near-sandbox python3 scripts/hire-demo.py
    python3 scripts/hire-demo.py --network testnet   # needs the keys in ~/.near-credentials

The agents here are scripted, and the market is a local stand-in: market.near.ai has a public catalog,
but hiring from it needs a marketplace account or mainnet USDC. See docs/AGENTS.md. What is real is
every grant, key, payment and refusal: those are signed transactions on the node.
"""
import hashlib
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "client"))

from espalier import near, policy, sandbox, world  # noqa: E402

MILLI = near.MILLI
EVIDENCE = os.path.join(HERE, "..", "docs", "evidence")
LINES = []


def say(text=""):
    print(text, flush=True)
    LINES.append(text)


class Market:
    """Stand-in for a job market. Each worker has a first quote and a floor it will not go below."""

    OFFERS = {
        "summary": [{"worker": "worker-a", "quote": 140, "floor": 120, "good_work": True}],
        "translation": [{"worker": "worker-b", "quote": 300, "floor": 260, "good_work": True}],
        "fact-check": [{"worker": "worker-c", "quote": 45, "floor": 30, "good_work": False}],
    }

    def counter(self, offer, price, reasoning):
        say(f"     counter to {offer['worker']}: {price} mNEAR, because {reasoning}")
        if price >= offer["floor"]:
            say(f"     {offer['worker']} accepts {price}")
            return price
        say(f"     {offer['worker']} holds at {offer['floor']}")
        return offer["floor"]


def deliverable(part, good):
    return f"{part}: findings with sources, checked against the brief." if good else ""


def passes(text, part):
    """The contractor's own check. A real one would be task specific."""
    return text.startswith(part) and "sources" in text


class Run:
    def __init__(self, w, gc, accounts, budget):
        self.w, self.gc, self.accounts, self.budget = w, gc, accounts, budget
        self.receipts = []

    def spent(self):
        return int(self.gc.view()["spent"])

    def receipt(self, task, seller, price, paid, passed, txs, note=""):
        r = {"task": task, "bought_from": seller, "price_mnear": price, "paid": paid, "work_passed_check": passed, "txs": txs, "note": note}
        self.receipts.append(r)
        say(f"     receipt: {task} from {seller} at {price} mNEAR, paid={paid}, check={passed}")

    def quote_flow(self, part, planned):
        """Returns the agreed price, or None after walking away."""
        offer = Market.OFFERS[part][0]
        say(f"  {part}: planned {planned} mNEAR, {offer['worker']} quotes {offer['quote']}")
        if offer["quote"] <= planned:
            return offer, offer["quote"]
        left = (self.budget - self.spent() // MILLI)
        price = int(planned * 1.1)
        agreed = Market().counter(offer, price, f"the plan was {planned} and {left} mNEAR is left for the whole job")
        if agreed > planned * 1.25:
            say(f"     {agreed} is too far above the plan of {planned}: walking away")
            return offer, None
        return offer, agreed

    def hire(self, offer, expenses_mnear, vendor):
        """A worker gets its own key and a sub-budget that can only pay the named vendor."""
        worker_key = near.Key.generate()
        terms = self.gc.child_terms(expenses_mnear * MILLI, expenses_mnear * MILLI, [vendor], methods=("request_spend", "execute_approved"))
        decision, tx = self.gc.hire(worker_key.public_str, terms)
        grant = decision.get("new_grant")
        if grant is None:
            raise RuntimeError(f"delegate refused: {decision}")
        say(f"     hired {offer['worker']}: grant {grant}, cap {expenses_mnear} mNEAR, may pay only {vendor}, tx {tx}")
        return self.w.worker(grant, worker_key, human=self.gc.human, log=lambda l: say("       worker wrapper: " + l)), tx


def main():
    network = "testnet" if "--network" in sys.argv and sys.argv[sys.argv.index("--network") + 1] == "testnet" else "sandbox"
    if network == "testnet":
        return run_testnet()
    pol = policy.Policy(young_secs=15, code_change_secs=15, known_after_payments=2, known_after_span_secs=2)
    with sandbox.Sandbox(port=3061) as sb:
        w = world.World(sb.rpc, sb.root, policy=pol).build()
        accounts = {
            "data-vendor": w.account("data-vendor", 1).account,
            "worker-a": w.account("worker-a", 1).account,
            "worker-b": w.account("worker-b", 1).account,
            "worker-c": w.account("worker-c", 1).account,
            "attacker": w.account("attacker", 1).account,
        }
        time.sleep(16)
        ok = story(w, accounts, "sandbox", budget=300, tag="sandbox")
        w.close()
    sys.exit(0 if ok else 1)


def run_testnet():
    home = os.path.expanduser("~/.near-credentials/testnet")
    base = "espalier-vault-1105.testnet"
    rpc = near.Rpc("https://test.rpc.fastnear.com")
    deploy = json.load(open(os.path.join(EVIDENCE, "testnet-deploy.json")))

    def signer(account):
        data = json.load(open(os.path.join(home, f"{account}.json")))
        return near.Signer(rpc, account, near.Key.from_string(data.get("private_key") or data["secret_key"]))

    pol = policy.Policy(young_secs=60, code_change_secs=60, known_after_payments=2, known_after_span_secs=2)
    w = world.World(rpc, signer(base), policy=pol, first_block=deploy.get("first_block", 1), archival=near.Rpc("https://archival-rpc.testnet.fastnear.com"))
    w.vault_id, w.registry_id = deploy["vault"], deploy["registry"]
    w.owner = signer(deploy["owner"])
    # The attestor's key stays with the other keys, outside the repository.
    path = os.path.join(home, "attestor.json")
    if not os.path.exists(path):
        json.dump({"secret_key": near.Key.generate().secret_str}, open(path, "w"))
    w.attestor_key = near.Key.from_string(json.load(open(path))["secret_key"])
    from espalier import attestor as att

    reporters = [base, deploy["owner"], f"r1.{base}"]
    screen = {"registry": w.registry_id, "reporters": reporters, "quorum": 2}
    world.must(w.owner.call(w.vault_id, "set_attestor", {"attestor": w.attestor_key.public_str}, tgas=30), "set attestor")
    world.must(w.owner.call(w.vault_id, "set_screen", {"screen": screen}, tgas=30), "set screen")
    w.attestor = att.Attestor(rpc, w.archival, w.vault_id, w.attestor_key, pol, first_block=w.first_block, cache_secs=60)
    w.server, w.attestor_url = att.serve(w.attestor)
    accounts = {
        "data-vendor": f"r0.{base}",
        "worker-a": f"r1.{base}",
        "worker-b": f"r2.{base}",
        "worker-c": f"r2.{base}",
        "attacker": base,
    }
    ok = story(w, accounts, "testnet", budget=300, tag="testnet")
    world.must(w.owner.call(w.vault_id, "set_screen", {"screen": None}, tgas=30), "clear screen")
    w.close()
    sys.exit(0 if ok else 1)


def story(w, accounts, network, budget, tag):
    say(f"network: {network}, vault {w.vault_id}")
    vendor = accounts["data-vendor"]
    approved = set(accounts[k] for k in ("data-vendor", "worker-a", "worker-b", "worker-c"))
    human = w.human(lambda req: req["recipient"] in approved)
    gc = w.root_agent(budget * MILLI, 150 * MILLI, human=human, log=lambda l: say("     wrapper: " + l), max_depth=2)
    run = Run(w, gc, accounts, budget)
    say(f"owner gives the contractor grant {gc.grant}: budget {budget} mNEAR, 150 per payment")
    say("the owner signs off new recipients, and can see and stop everything: the contractor holds one function-call key")

    say("\n1. Task: research brief. Parts: dataset (buy), summary (hire), translation (hire), fact-check (hire)")
    r = gc.pay(vendor, 30 * MILLI, "dataset")
    run.receipt("dataset", vendor, 30, r.paid, True, r.txs)

    say("\n2. Summary: the quote is above plan, so the contractor counters once")
    offer, price = run.quote_flow("summary", 100)
    worker, tx = run.hire(offer, 20, vendor)
    hire_txs = [tx]
    time.sleep(3)
    r = worker.pay(vendor, 15 * MILLI, "extra source for the summary")
    say(f"     worker bought a source from its sub-budget: {r.status}")
    hire_txs += r.txs

    say("\n3. The worker reads an injected page and goes rogue. Each attempt is a real transaction or a refusal to sign.")
    attacker = accounts["attacker"]
    raw = worker.signer.call(w.vault_id, "request_spend", {"recipient": attacker, "amount": str(5 * MILLI)}, tgas=120)
    say(f"     pay the attacker: {raw.value['outcome']} {raw.value['code']}, tx {raw.hash}")
    raw2 = worker.signer.call(w.vault_id, "request_spend", {"recipient": vendor, "amount": str(50 * MILLI)}, tgas=120)
    say(f"     overspend its budget: {raw2.value['outcome']} {raw2.value['code']}, tx {raw2.hash}")
    try:
        worker.hire(near.Key.generate().public_str, gc.child_terms(MILLI, MILLI, [attacker]))
        say("     delegate: went through, which is a failure of this demo")
        bad_delegate = False
    except near.RpcError as e:
        say(f"     hire a friend: the protocol refused the transaction: {str(e.data)[:110]}")
        bad_delegate = True

    text = deliverable("summary", offer["good_work"])
    passed = passes(text, "summary")
    r = gc.pay(accounts["worker-a"], price * MILLI, "summary fee")
    run.receipt("summary", accounts["worker-a"], price, r.paid, passed, hire_txs + r.txs, "countered once")
    _, tx = gc.revoke(worker.grant)
    say(f"     contractor revokes the worker's grant, tx {tx}")
    time.sleep(3)
    try:
        worker.signer.call(w.vault_id, "request_spend", {"recipient": vendor, "amount": str(MILLI)}, tgas=120)
        revoked_ok = False
        say("     the revoked key still signed, which is a failure of this demo")
    except near.RpcError as e:
        revoked_ok = True
        say(f"     the revoked key no longer exists: {str(e.data)[:90]}")

    say("\n4. Translation: the worker will not come down, so the contractor walks away and does it itself")
    offer, price = run.quote_flow("translation", 80)
    run.receipt("translation", offer["worker"] if price else "self", price or 0, False, True, [], "walked away: no payment")

    say("\n5. Fact-check: hired, but the work fails the contractor's check, so no fee is paid")
    offer, price = run.quote_flow("fact-check", 50)
    worker, tx = run.hire(offer, 5, vendor)
    text = deliverable("fact-check", offer["good_work"])
    passed = passes(text, "fact-check")
    _, rtx = gc.revoke(worker.grant)
    run.receipt("fact-check", accounts["worker-c"], price, False, passed, [tx, rtx], "work rejected, grant revoked")

    time.sleep(3)
    me = gc.view()
    spent = int(me["spent"])
    say(f"\nresult: tree spent {spent / MILLI:.0f} of {budget} mNEAR, read from the contract. Denied attempts on the worker were {raw.value['code']} and {raw2.value['code']}.")
    say(f"the owner was asked {len(human.asked)} times")
    ok = (
        spent <= budget * MILLI
        and raw.value["code"] == "RECIPIENT_NOT_ALLOWED"
        and raw2.value["outcome"] == "denied"
        and bad_delegate
        and revoked_ok
    )
    doc = {
        "network": network,
        "vault": w.vault_id,
        "budget_mnear": budget,
        "spent_mnear": spent / MILLI,
        "receipts": run.receipts,
        "agents_are_scripted": True,
        "market_is_a_local_stand_in": True,
        "rogue_worker": {
            "pay_attacker": {"code": raw.value["code"], "tx": raw.hash},
            "overspend": {"code": raw2.value["code"], "tx": raw2.hash},
            "delegate_refused_by_protocol": bad_delegate,
            "revoked_key_gone": revoked_ok,
        },
        "human_questions": len(human.asked),
        "passed": ok,
    }
    world.dump(os.path.join(EVIDENCE, f"hire-demo-{tag}.json"), doc)
    with open(os.path.join(EVIDENCE, f"hire-demo-{tag}.log"), "w", encoding="utf8") as f:
        f.write("\n".join(LINES) + "\n")
    print(hashlib.sha256("\n".join(LINES).encode()).hexdigest()[:12], "log written")
    return ok


main()
