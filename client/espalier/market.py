"""Client for the Espalier market, standard library only.

A poster signs with its grant key, the same key that calls `delegate` on the vault. Anyone can carry
the signed message to the market. The byte layouts here are the ones in market/src/lib.rs, and the
client tests compare them with what the contract's own views return.
"""
import hashlib
import json
import re

from . import near

TAG_POST = b"espalier-market-post-v1"
TAG_ACCEPT = b"espalier-market-accept-v1"
TAG_REJECT = b"espalier-market-reject-v1"
TAG_BID = b"espalier-market-bid-v1"

SPEC_FIELDS = (
    "vault", "grant_id", "spec_hash", "max_budget", "bid_bond", "min_bids", "commit_end_ms", "reveal_end_ms",
    "award_end_ms", "deliver_end_ms", "review_end_ms", "sub_depth", "sub_nodes", "sub_gas", "key_allowance", "nonce",
)


def spec_bytes(spec):
    """Borsh of a Spec, field by field in declaration order."""
    s = spec
    return (
        near.bstr(s["vault"])
        + near.u64(int(s["grant_id"]))
        + near.bstr(s["spec_hash"])
        + near.u128(int(s["max_budget"]))
        + near.u128(int(s["bid_bond"]))
        + near.u8(int(s["min_bids"]))
        + b"".join(near.u64(int(s[k])) for k in ("commit_end_ms", "reveal_end_ms", "award_end_ms", "deliver_end_ms", "review_end_ms"))
        + near.u8(int(s["sub_depth"]))
        + near.u32(int(s["sub_nodes"]))
        + near.u128(int(s["sub_gas"]))
        + near.u128(int(s["key_allowance"]))
        + near.u64(int(s["nonce"]))
    )


def post_message(market, spec):
    return TAG_POST + near.bstr(market) + spec_bytes(spec)


def accept_message(market, task_id):
    return TAG_ACCEPT + near.bstr(market) + near.u64(task_id)


def reject_message(market, task_id, reason_hash):
    return TAG_REJECT + near.bstr(market) + near.u64(task_id) + near.bstr(reason_hash)


def bid_commitment(market, task_id, worker, price, pk, salt):
    """`salt` is 32 raw bytes. The hash names the market, the task and the worker."""
    body = near.bstr(market) + near.u64(task_id) + near.bstr(worker) + near.u128(int(price)) + near.bstr(pk) + salt
    return hashlib.sha256(TAG_BID + body).hexdigest()


def pick(bids, max_budget, min_bids):
    """The pricing rule. `bids` is a list of (price, commit sequence). Returns (index, price) or None.
    Lowest price wins, earlier commitment on a tie, and it is paid the second-lowest price, or the
    maximum budget when it is alone."""
    if not bids or len(bids) < max(1, min_bids):
        return None
    order = sorted(range(len(bids)), key=lambda i: (bids[i][0], bids[i][1]))
    price = min(bids[order[1]][0], max_budget) if len(order) > 1 else max_budget
    return order[0], price


def code_of(outcome):
    """The reason code of a market call: from the value it returned, or from its panic text."""
    if outcome.ok:
        v = outcome.value
        return v.get("code", "ok") if isinstance(v, dict) else "ok"
    m = re.search(r"panicked: ([A-Z_]+)", outcome.failure() or "")
    return m.group(1) if m else "FAILED"


class Market:
    """Calls on one market contract. Every sender is a `near.Signer` for a plain account: a worker's
    own account, or whichever account carries a poster's signed message."""

    def __init__(self, rpc, market):
        self.rpc = rpc
        self.id = market

    def view(self, method, **args):
        return self.rpc.view(self.id, method, args)

    def task(self, task_id):
        return self.view("get_task", task_id=task_id)

    def owed(self, account):
        return int(self.view("balance_of", account_id=account))

    def post(self, carrier, spec, grant_key, bond):
        sig = grant_key.sign(post_message(self.id, spec)).hex()
        return carrier.call(self.id, "post_task", {"spec": spec, "signature": sig}, tgas=70, deposit=bond)

    def commit(self, worker, task_id, price, pk, salt, bond):
        c = bid_commitment(self.id, task_id, worker.account, price, pk, salt)
        return worker.call(self.id, "commit_bid", {"task_id": task_id, "commitment": c}, tgas=20, deposit=bond)

    def reveal(self, worker, task_id, price, pk, salt):
        return worker.call(self.id, "reveal_bid", {"task_id": task_id, "price": str(price), "pk": pk, "salt": salt.hex()}, tgas=60)

    def close(self, anyone, task_id):
        return anyone.call(self.id, "close_bidding", {"task_id": task_id}, tgas=30)

    def confirm(self, anyone, task_id, grant_id):
        return anyone.call(self.id, "confirm_award", {"task_id": task_id, "grant_id": grant_id}, tgas=60)

    def default(self, anyone, task_id):
        return anyone.call(self.id, "default_award", {"task_id": task_id}, tgas=120)

    def deliver(self, worker, task_id, result_hash):
        return worker.call(self.id, "deliver", {"task_id": task_id, "result_hash": result_hash}, tgas=15)

    def accept(self, carrier, task_id, grant_key):
        sig = grant_key.sign(accept_message(self.id, task_id)).hex()
        return carrier.call(self.id, "accept", {"task_id": task_id, "signature": sig}, tgas=60)

    def reject(self, carrier, task_id, reason_hash, grant_key):
        sig = grant_key.sign(reject_message(self.id, task_id, reason_hash)).hex()
        return carrier.call(self.id, "reject", {"task_id": task_id, "reason_hash": reason_hash, "signature": sig}, tgas=60)

    def fail_delivery(self, anyone, task_id):
        return anyone.call(self.id, "fail_delivery", {"task_id": task_id}, tgas=60)

    def void(self, anyone, task_id):
        return anyone.call(self.id, "void_task", {"task_id": task_id}, tgas=60)

    def claim(self, anyone, task_id):
        return anyone.call(self.id, "claim", {"task_id": task_id}, tgas=60)

    def refund(self, anyone, task_id):
        return anyone.call(self.id, "refund", {"task_id": task_id}, tgas=20)

    def payout(self, anyone, account):
        return anyone.call(self.id, "payout", {"account_id": account}, tgas=25)


def award_terms(spec, price, poster_view, market):
    """The terms of the grant a task promises its winner, ready for the vault's `delegate`."""
    depth = int(spec["sub_depth"])
    return {
        "asset": "Native",
        "cap": str(price),
        "per_tx": str(price),
        "window_cap": str(price),
        "window_secs": poster_view["window_secs"],
        "approval_threshold": str(price),
        "recipients": [market],
        "methods": ["request_spend", "delegate", "revoke"] if depth > 0 else ["request_spend"],
        "expires_at_ms": poster_view["expires_at_ms"],
        "max_depth": depth,
        "max_denials": poster_view["max_denials"],
        "key_allowance": str(spec["key_allowance"]),
        "gas_budget": str(spec["sub_gas"]),
        "node_budget": int(spec["sub_nodes"]),
        "clearance_required": False,
    }


def dumps(doc):
    return json.dumps(doc, indent=2) + "\n"
