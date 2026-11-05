"""What an agent spends through. The agent holds one function-call key on the vault and nothing else.

    pay(recipient, amount)
        ask the recipient check
        block -> no transaction is signed at all
        warn  -> a human is asked; a no ends here, a yes goes through the vault's approval queue
        allow -> the spend is signed with the attestor's ticket

The wrapper is the polite path, not the guarantee. An agent that skips it, or a stolen key, reaches
`request_spend` with no ticket: on a clearance-bound grant that queues for a human, and every limit,
the allowlist and the denylist still apply on chain. Age and code signals are the only part a skipped
wrapper loses, and only for spends that a human then chooses to approve.
"""
import json
import time
import urllib.request
from dataclasses import dataclass, field

from . import near

TGAS_SPEND = 120


@dataclass
class Result:
    """How a spend ended. `status` is one of: paid, refused_block, refused_human, refused_chain,
    rolled_back, queued, check_failed."""
    status: str
    recipient: str
    amount: int
    verdict: str = ""
    reasons: list = field(default_factory=list)
    decision: dict = field(default_factory=dict)
    txs: list = field(default_factory=list)
    signed: bool = False
    signals: dict = field(default_factory=dict)

    @property
    def paid(self):
        return self.status == "paid"

    def as_dict(self):
        return {
            "status": self.status,
            "recipient": self.recipient,
            "amount": str(self.amount),
            "verdict": self.verdict,
            "reasons": self.reasons,
            "contract_code": self.decision.get("code", ""),
            "contract_detail": self.decision.get("detail", ""),
            "signed_a_transaction": self.signed,
            "txs": self.txs,
            "signals": self.signals,
        }


def http_checker(url):
    """Asks an attestor over HTTP. Any failure comes back as warn with no ticket, never as allow."""

    def check(grant, recipient, amount):
        body = json.dumps({"grant": grant, "recipient": recipient, "amount": str(amount)}).encode()
        req = urllib.request.Request(url, body, {"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                return json.load(r)
        except OSError as e:
            return {"verdict": "warn", "reasons": [{"level": "warn", "code": "SIGNAL_MISSING", "detail": f"check failed: {e}"}]}

    return check


class Agent:
    def __init__(self, rpc, vault, key, grant, check, human=None, log=None):
        """`check` is `http_checker(url)` or anything with that shape. `human` has `review(request)`,
        which says yes or no, and `approve(approval_id, spend_hash)`, which the human does with their
        own key: the wrapper never holds a key that can approve. No human means warn is a no."""
        self.signer = near.Signer(rpc, vault, key)
        self.rpc = rpc
        self.vault = vault
        self.key = key
        self.grant = grant
        self.check = check
        self.human = human
        self.log = log or (lambda line: None)

    def _spend(self, recipient, amount, memo, clearance=None):
        args = {"recipient": recipient, "amount": str(amount), "memo": memo}
        if clearance is not None:
            args["clearance"] = clearance
        return self.signer.call(self.vault, "request_spend", args, tgas=TGAS_SPEND)

    def _finish(self, out, decision, tx_hash):
        out.decision = decision or {}
        out.signed = True
        out.txs.append(tx_hash)
        kind = out.decision.get("outcome")
        if kind == "settled":
            out.status = "paid"
        elif kind == "rolled_back":
            out.status = "rolled_back"
        elif kind == "queued":
            out.status = "queued"
        else:
            out.status = "refused_chain"
        return out

    def pay(self, recipient, amount, memo=None):
        v = self.check(self.grant, recipient, amount)
        verdict = v.get("verdict", "warn")
        out = Result("check_failed", recipient, amount, verdict=verdict, reasons=v.get("reasons", []), signals=v.get("signals", {}))
        codes = ",".join(r["code"] for r in out.reasons if r["level"] != "info") or "clean"
        self.log(f"check {recipient} -> {verdict} [{codes}]")
        if verdict == "block":
            out.status = "refused_block"
            self.log("refused to sign: no transaction was created")
            return out
        if verdict == "warn":
            return self._ask_human(out, memo, v)
        r = self._spend(recipient, amount, memo, v.get("ticket"))
        self._finish(out, r.value, r.hash)
        self.log(f"signed with ticket: {out.status} {out.decision.get('code', '')} tx {r.hash}")
        return out

    def _ask_human(self, out, memo, v):
        if self.human is None:
            out.status = "refused_human"
            self.log("warn with no human available: not signing")
            return out
        request = {"recipient": out.recipient, "amount": str(out.amount), "memo": memo, "reasons": out.reasons}
        if not self.human.review(request):
            out.status = "refused_human"
            self.log("human said no: not signing")
            return out
        # A yes still goes through the chain: the queue entry is what the human's own key approves.
        r = self._spend(out.recipient, out.amount, memo)
        self._finish(out, r.value, r.hash)
        d = out.decision
        if d.get("outcome") != "queued":
            return out
        out.txs.append(self.human.approve(d["approval_id"], d["spend_hash"]))
        r = self.signer.call(
            self.vault,
            "execute_approved",
            {"approval_id": d["approval_id"], "recipient": out.recipient, "amount": str(out.amount), "memo": memo},
            tgas=TGAS_SPEND,
        )
        self._finish(out, r.value, r.hash)
        self.log(f"human approved, executed: {out.status} {out.decision.get('code', '')} tx {r.hash}")
        return out

    def view(self):
        return self.rpc.view(self.vault, "get_grant", {"grant_id": self.grant})

    # ---- Delegation -----------------------------------------------------------------------------

    def hire(self, worker_public_key, terms):
        """Gives a worker a sub-budget. The worker made its own key and sent only the public half, so
        this agent could not spend as the worker even if it wanted to. The vault refuses terms wider
        than this agent's own, whatever this code asks for."""
        r = self.signer.call(self.vault, "delegate", {"child_pk": worker_public_key, "terms": terms}, tgas=100)
        return r.value or {}, r.hash

    def revoke(self, grant):
        r = self.signer.call(self.vault, "revoke", {"grant_id": grant}, tgas=100)
        return r.value or {}, r.hash

    def child_terms(self, cap, per_tx, recipients, seconds=3600, methods=("request_spend",), max_denials=None, key_allowance=None):
        """Terms that are narrower than this agent's grant, ready for `hire`."""
        me = self.view()
        now = self.rpc.view(self.vault, "vault_info")["now_ms"]
        allowance = key_allowance if key_allowance is not None else 400 * near.MILLI
        return {
            "asset": me["asset"],
            "cap": str(cap),
            "per_tx": str(per_tx),
            "window_cap": str(min(cap, int(me["window_cap"]))),
            "window_secs": me["window_secs"],
            "approval_threshold": str(min(per_tx, int(me["approval_threshold"]))),
            "recipients": recipients,
            "methods": list(methods),
            "expires_at_ms": min(int(me["expires_at_ms"]), now + seconds * 1000),
            "max_depth": 0,
            "max_denials": min(max_denials or me["max_denials"], me["max_denials"]),
            "key_allowance": str(allowance),
            "gas_budget": "0",
            "node_budget": 0,
            "clearance_required": me["clearance_required"],
        }


def wait_until(predicate, timeout=30, step=0.5):
    end = time.time() + timeout
    while time.time() < end:
        if predicate():
            return True
        time.sleep(step)
    return False
