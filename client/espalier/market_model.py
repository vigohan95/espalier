"""A model of the vault and the market together, in plain Python, with no chain.

It is written from the rules in docs/SWARM-RULES.md, independently of the contracts, and answers every
call with the same reason codes. The simulation runs on it in `--offline` mode, and when the same
script runs on a real node the two results are compared: a difference is a bug in one of the three.

Only what the market's chain of grants uses is modelled: native NEAR, one recipient per grant, caps
charged to every ancestor, delegation that can only narrow, and revocation of a whole branch.
"""
from .market import pick

FINAL = ("Paid", "Rejected", "Failed", "NoAward", "Defaulted", "Void")
MAX_DEFAULTS = 3
MAX_OPEN_TASKS = 8
MAX_BIDS = 16
MIN_DENIALS = 3


class Model:
    def __init__(self, market="market", vault="vault", owner="owner", poster_bond=0, min_window_ms=1000, draw_grace_ms=1000):
        self.market, self.vault, self.owner = market, vault, owner
        self.poster_bond, self.min_window_ms, self.draw_grace_ms = poster_bond, min_window_ms, draw_grace_ms
        self.now = 0
        self.grants = {}
        self.keys = {}
        self.tasks = {}
        self.roster = None
        self.used_keys = set()
        self.grant_task = {}
        self.posters = {}
        self.seen = {}
        self.balances = {}
        self.posted = set()
        self.bonds_held = 0
        self.held_total = 0
        self.received = {}

    # ---- The vault ---------------------------------------------------------------------------------

    def create_root(self, key, terms):
        return self._grant(None, key, terms)

    def _grant(self, parent, key, t):
        gid = len(self.grants) + 1
        self.grants[gid] = {
            "id": gid, "parent": parent, "pk": key, "cap": int(t["cap"]), "per_tx": int(t["per_tx"]),
            "window_cap": int(t["window_cap"]), "approval_threshold": int(t["approval_threshold"]),
            "recipients": list(t["recipients"]) if t["recipients"] is not None else None, "methods": list(t["methods"]),
            "expires_at_ms": t["expires_at_ms"], "max_depth": t["max_depth"], "max_denials": t["max_denials"],
            "key_allowance": int(t["key_allowance"]), "gas_left": int(t["gas_budget"]), "nodes_left": t["node_budget"],
            "clearance_required": t["clearance_required"], "spent": 0, "direct_spent": 0, "denied": 0,
            "revoked": False, "key_live": True, "children": [],
        }
        self.keys[key] = gid
        if parent is not None:
            p = self.grants[parent]
            p["children"].append(gid)
            p["gas_left"] -= int(t["key_allowance"]) + int(t["gas_budget"])
            p["nodes_left"] -= t["node_budget"] + 1
        return gid

    def lineage(self, gid):
        out = []
        while gid is not None:
            out.append(gid)
            gid = self.grants[gid]["parent"]
        return out

    def live(self, gid):
        return self._dead(gid) is None

    def _dead(self, gid):
        for i in self.lineage(gid):
            g = self.grants[i]
            if g["revoked"]:
                return "REVOKED" if i == gid else "ANCESTOR_REVOKED"
            if self.now >= g["expires_at_ms"]:
                return "EXPIRED" if i == gid else "ANCESTOR_EXPIRED"
        return None

    def spendable(self, gid):
        if not self.live(gid):
            return 0
        return min(self.grants[i]["cap"] - self.grants[i]["spent"] for i in self.lineage(gid))

    def view(self, gid):
        g = self.grants.get(gid)
        if g is None:
            return None
        return dict(g, live=self.live(gid), spendable=self.spendable(gid))

    def _deny(self, gid, code):
        g = self.grants[gid]
        g["denied"] += 1
        if code in ("REVOKED", "ANCESTOR_REVOKED", "EXPIRED", "ANCESTOR_EXPIRED"):
            self._drop_key(gid)
        elif g["denied"] >= g["max_denials"] and not g["revoked"]:
            self._revoke(gid)
        return code

    def _drop_key(self, gid):
        g = self.grants[gid]
        if g["key_live"]:
            g["key_live"] = False
            self.keys.pop(g["pk"], None)

    def _revoke(self, gid):
        self.grants[gid]["revoked"] = True
        stack = [gid]
        while stack:
            i = stack.pop()
            self._drop_key(i)
            stack += self.grants[i]["children"]

    def delegate(self, parent, key, t):
        """Returns (code, new grant id). Checks in the vault's order."""
        if not self.grants[parent]["key_live"]:
            return "NO_KEY", None
        p = self.grants[parent]
        if "delegate" not in p["methods"]:
            return "METHOD_NOT_GRANTED", None
        dead = self._dead(parent)
        if dead:
            return self._deny(parent, dead), None
        code = None
        remaining = min(self.grants[i]["cap"] - self.grants[i]["spent"] for i in self.lineage(parent))
        if int(t["cap"]) > remaining:
            code = "ATT_CAP"
        elif int(t["per_tx"]) > p["per_tx"]:
            code = "ATT_PER_TX"
        elif int(t["window_cap"]) > p["window_cap"]:
            code = "ATT_WINDOW_CAP"
        elif int(t["approval_threshold"]) > p["approval_threshold"]:
            code = "ATT_THRESHOLD"
        elif p["recipients"] is not None and (t["recipients"] is None or any(r not in p["recipients"] for r in t["recipients"])):
            code = "ATT_RECIPIENTS"
        elif any(m not in p["methods"] for m in t["methods"]):
            code = "ATT_METHODS"
        elif t["expires_at_ms"] > p["expires_at_ms"]:
            code = "ATT_EXPIRY"
        elif p["max_depth"] == 0 or t["max_depth"] > p["max_depth"] - 1:
            code = "ATT_DEPTH"
        elif t["max_denials"] > p["max_denials"]:
            code = "ATT_DENIALS"
        elif int(t["key_allowance"]) + int(t["gas_budget"]) > p["gas_left"]:
            code = "ATT_GAS"
        elif t["node_budget"] + 1 > p["nodes_left"]:
            code = "ATT_NODES"
        elif key in self.keys:
            code = "KEY_IN_USE"
        if code:
            return self._deny(parent, code), None
        return "GRANTED", self._grant(parent, key, t)

    def spend(self, gid, recipient, amount):
        """request_spend. Returns (code, blocking grant)."""
        g = self.grants[gid]
        if not g["key_live"]:
            return "NO_KEY", None
        dead = self._dead(gid)
        if dead:
            return self._deny(gid, dead), None
        if g["recipients"] is not None and recipient not in g["recipients"]:
            return self._deny(gid, "RECIPIENT_NOT_ALLOWED"), gid
        if amount > g["per_tx"]:
            return self._deny(gid, "PER_TX"), gid
        for i in self.lineage(gid):
            a = self.grants[i]
            if amount > a["cap"] - a["spent"]:
                return self._deny(gid, "CAP" if i == gid else "ANCESTOR_CAP"), i
        for i in self.lineage(gid):
            self.grants[i]["spent"] += amount
        g["direct_spent"] += amount
        return "SETTLED", None

    def revoke(self, by, target):
        """`by` is "owner" or a grant id. Only a strict ancestor may revoke."""
        if by != "owner":
            if not self.grants[by]["key_live"]:
                return "NO_KEY"
            dead = self._dead(by)
            if dead:
                return self._deny(by, dead)
            if by not in self.lineage(target)[1:]:
                return self._deny(by, "NOT_ANCESTOR")
        if self.grants[target]["revoked"]:
            return "ALREADY_REVOKED"
        self._revoke(target)
        return "REVOKED"

    # ---- The market --------------------------------------------------------------------------------

    def _credit(self, account, amount):
        if amount:
            self.balances[account] = self.balances.get(account, 0) + amount

    def _stat(self, gid):
        return self.posters.setdefault(gid, {"posted": 0, "open": 0, "committed": 0, "defaults": 0, "rejected": 0, "accepted": 0})

    @staticmethod
    def _commitment(t):
        if t["status"] == "Open":
            return True, t["spec"]["max_budget"]
        if t["status"] in ("Awarding", "Awarded", "Delivered", "Accepted"):
            return True, t["price"] - t["drawn"]
        return False, 0

    def _settle(self, t, before):
        after = self._commitment(t)
        s = self._stat(t["spec"]["grant_id"])
        s["committed"] += after[1] - before[1]
        if before[0] and not after[0]:
            s["open"] -= 1

    def set_roster(self, caller, workers):
        if caller != self.owner:
            return "NOT_OWNER"
        self.roster = list(workers)
        return "ROSTER_SET"

    def post(self, spec, signer_key, bond, payer):
        """`signer_key` stands for the signature: the key that signed. Returns (code, task id)."""
        s = dict(spec, max_budget=int(spec["max_budget"]), bid_bond=int(spec["bid_bond"]), sub_gas=int(spec["sub_gas"]), key_allowance=int(spec["key_allowance"]))
        if bond != self.poster_bond:
            return "WRONG_BOND", None
        if s["vault"] != self.vault:
            return "VAULT_NOT_LISTED", None
        if self.roster is None:
            return "NO_ROSTER", None
        steps = [self.now, s["commit_end_ms"], s["reveal_end_ms"], s["award_end_ms"], s["deliver_end_ms"], s["review_end_ms"]]
        if s["max_budget"] == 0 or s["bid_bond"] == 0:
            return "BAD_SPEC", None
        if any(b < a + self.min_window_ms for a, b in zip(steps, steps[1:])):
            return "BAD_WINDOWS", None
        g = self.view(s["grant_id"])
        digest = repr(sorted(s.items()))
        code = None
        if g is None:
            code = "UNKNOWN_GRANT"
        elif digest in self.posted:
            code = "DUPLICATE_TASK"
        elif not g["live"]:
            code = "GRANT_NOT_LIVE"
        elif signer_key != g["pk"]:
            code = "BAD_SIGNATURE"
        elif g["recipients"] != [self.market]:
            code = "RECIPIENTS_NOT_MARKET_ONLY"
        elif g["clearance_required"]:
            code = "CLEARANCE_BOUND"
        elif "delegate" not in g["methods"]:
            code = "CANNOT_DELEGATE"
        elif g["max_depth"] < s["sub_depth"] + 1:
            code = "DEPTH_SHORT"
        elif g["nodes_left"] < s["sub_nodes"] + 1:
            code = "NODES_SHORT"
        elif g["gas_left"] < s["key_allowance"] + s["sub_gas"]:
            code = "GAS_SHORT"
        elif g["max_denials"] < MIN_DENIALS:
            code = "DENIALS_SHORT"
        elif g["expires_at_ms"] < s["review_end_ms"] + self.draw_grace_ms:
            code = "EXPIRES_TOO_SOON"
        elif min(g["per_tx"], g["window_cap"], g["approval_threshold"]) < s["max_budget"]:
            code = "LIMITS_SHORT"
        else:
            st = self._stat(s["grant_id"])
            if st["defaults"] >= MAX_DEFAULTS:
                code = "POSTER_BANNED"
            elif st["open"] >= MAX_OPEN_TASKS:
                code = "TOO_MANY_OPEN"
            elif g["spendable"] < st["committed"] + s["max_budget"]:
                code = "OVERCOMMITTED"
        if code:
            return code, None
        self.posted.add(digest)
        tid = len(self.tasks) + 1
        st = self._stat(s["grant_id"])
        st["posted"] += 1
        st["open"] += 1
        st["committed"] += s["max_budget"]
        self.bonds_held += bond
        self.tasks[tid] = {
            "id": tid, "spec": s, "status": "Open", "note": "", "bond_payer": payer, "poster_bond": bond, "bids": [],
            "winner": None, "price": 0, "grant": None, "drawn": 0, "paid": 0, "held": 0, "refunded": 0,
        }
        return "POSTED", tid

    def commit(self, worker, tid, commitment, bond):
        """`commitment` is the tuple (price, key, salt) the worker will reveal: the model keeps it sealed."""
        t = self.tasks[tid]
        if t["status"] != "Open" or self.now >= t["spec"]["commit_end_ms"]:
            return "WRONG_PHASE"
        if bond != t["spec"]["bid_bond"]:
            return "WRONG_BOND"
        if worker not in (self.roster or []):
            return "NOT_ON_ROSTER"
        if any(b["worker"] == worker for b in t["bids"]):
            return "ALREADY_BID"
        if len(t["bids"]) >= MAX_BIDS:
            return "TASK_FULL"
        self.bonds_held += bond
        t["bids"].append({"worker": worker, "sealed": commitment, "bond": bond, "price": None, "pk": None})
        return "ok"

    def reveal(self, worker, tid, price, key, salt):
        t = self.tasks[tid]
        if t["status"] != "Open" or not (t["spec"]["commit_end_ms"] <= self.now < t["spec"]["reveal_end_ms"]):
            return "WRONG_PHASE"
        bid = next((b for b in t["bids"] if b["worker"] == worker), None)
        if bid is None:
            return "NO_BID"
        if bid["price"] is not None:
            return "ALREADY_REVEALED"
        if bid["sealed"] != (price, key, salt):
            return "BAD_REVEAL"
        if price == 0 or price > t["spec"]["max_budget"]:
            return "PRICE_OUT_OF_RANGE"
        if key in self.used_keys or key in self.keys:
            return "KEY_TAKEN"
        bid["price"], bid["pk"] = price, key
        self.used_keys.add(key)
        return "REVEALED"

    def close(self, tid):
        t = self.tasks[tid]
        if t["status"] != "Open" or self.now < t["spec"]["reveal_end_ms"]:
            return "WRONG_PHASE", None
        before = self._commitment(t)
        for b in t["bids"]:
            if b["price"] is None:
                self.bonds_held -= b["bond"]
                self._credit(self.vault, b["bond"])
                b["bond"] = 0
        valid = [(i, b["price"]) for i, b in enumerate(t["bids"]) if b["price"] is not None]
        got = pick([(p, i) for i, p in valid], t["spec"]["max_budget"], t["spec"]["min_bids"])
        if got is None:
            for i, _ in valid:
                self._release_bid(t, i)
            self._release_poster_bond(t)
            t["status"], t["note"] = "NoAward", f"{len(valid)} valid bids, {max(1, t['spec']['min_bids'])} needed"
            self._settle(t, before)
            return "NO_AWARD", None
        winner = valid[got[0]][0]
        for i, _ in valid:
            if i != winner:
                self._release_bid(t, i)
        t["winner"], t["price"], t["status"] = winner, got[1], "Awarding"
        self._settle(t, before)
        return "WINNER", got[1]

    def _release_bid(self, t, i, to=None):
        b = t["bids"][i]
        self.bonds_held -= b["bond"]
        self._credit(to or b["worker"], b["bond"])
        b["bond"] = 0

    def _release_poster_bond(self, t, to=None):
        self.bonds_held -= t["poster_bond"]
        self._credit(to or t["bond_payer"], t["poster_bond"])
        t["poster_bond"] = 0

    def confirm(self, tid, gid):
        t = self.tasks[tid]
        if t["status"] != "Awarding":
            return "WRONG_PHASE"
        g = self.view(gid)
        if g is None:
            return "UNKNOWN_GRANT"
        s, bid = t["spec"], t["bids"][t["winner"]]
        need = ["request_spend"] + (["delegate", "revoke"] if s["sub_depth"] > 0 else [])
        code = None
        if g["parent"] != s["grant_id"]:
            code = "WRONG_PARENT"
        elif g["pk"] != bid["pk"]:
            code = "WRONG_KEY"
        elif not g["live"] or not g["key_live"]:
            code = "GRANT_NOT_LIVE"
        elif g["recipients"] != [self.market]:
            code = "RECIPIENTS_NOT_MARKET_ONLY"
        elif g["clearance_required"]:
            code = "CLEARANCE_BOUND"
        elif g["cap"] != t["price"]:
            code = "WRONG_CAP"
        elif not (g["per_tx"] == g["window_cap"] == g["approval_threshold"] == t["price"]):
            code = "WRONG_LIMITS"
        elif any(m not in g["methods"] for m in need):
            code = "METHODS_SHORT"
        elif g["max_depth"] < s["sub_depth"]:
            code = "DEPTH_SHORT"
        elif g["nodes_left"] < s["sub_nodes"]:
            code = "NODES_SHORT"
        elif g["gas_left"] < s["sub_gas"] or g["key_allowance"] < s["key_allowance"]:
            code = "GAS_SHORT"
        elif g["max_denials"] < MIN_DENIALS:
            code = "DENIALS_SHORT"
        elif g["expires_at_ms"] < s["review_end_ms"] + self.draw_grace_ms:
            code = "EXPIRES_TOO_SOON"
        elif gid in self.grant_task:
            code = "GRANT_BOUND"
        if code:
            return code
        before = self._commitment(t)
        self.grant_task[gid] = tid
        self.seen[gid] = g["direct_spent"]
        t["grant"], t["status"] = gid, "Awarded"
        self._release_poster_bond(t)
        self._settle(t, before)
        return "AWARDED"

    def _void(self, t, reason):
        before = self._commitment(t)
        for i, b in enumerate(t["bids"]):
            if b["bond"]:
                self._release_bid(t, i)
        self._release_poster_bond(t)
        t["status"], t["note"] = "Void", reason
        self._settle(t, before)
        return "VOID"

    def default(self, tid):
        t = self.tasks[tid]
        if t["status"] != "Awarding" or self.now < t["spec"]["award_end_ms"]:
            return "WRONG_PHASE"
        poster = t["spec"]["grant_id"]
        if not self.live(poster):
            return self._void(t, "POSTER_NOT_LIVE")
        holder = self.keys.get(t["bids"][t["winner"]]["pk"])
        if holder is not None and self.grants[holder]["parent"] != poster:
            return self._void(t, "KEY_TAKEN_BY_ANOTHER_GRANT")
        before = self._commitment(t)
        worker = t["bids"][t["winner"]]["worker"]
        self._release_poster_bond(t, to=worker)
        self._release_bid(t, t["winner"])
        t["status"], t["note"] = "Defaulted", "no grant was created for the winner" if holder is None else "wrong terms"
        self._stat(poster)["defaults"] += 1
        self._settle(t, before)
        return "DEFAULTED"

    def deliver(self, worker, tid):
        t = self.tasks[tid]
        if t["status"] != "Awarded" or self.now >= t["spec"]["deliver_end_ms"]:
            return "WRONG_PHASE"
        if worker != t["bids"][t["winner"]]["worker"]:
            return "NOT_WINNER"
        t["status"] = "Delivered"
        return "ok"

    def review(self, tid, accepted, signer_key):
        t = self.tasks[tid]
        if t["status"] != "Delivered" or (not accepted and self.now >= t["spec"]["review_end_ms"]):
            return "WRONG_PHASE"
        poster = self.view(t["spec"]["grant_id"])
        if not poster["live"]:
            return "GRANT_NOT_LIVE"
        if signer_key != poster["pk"]:
            return "BAD_SIGNATURE"
        before = self._commitment(t)
        self._release_bid(t, t["winner"])
        st = self._stat(t["spec"]["grant_id"])
        if accepted:
            t["status"] = "Accepted"
            st["accepted"] += 1
        else:
            t["status"], t["note"] = "Rejected", "rejected by the poster"
            st["rejected"] += 1
        self._settle(t, before)
        return "ACCEPTED" if accepted else "REJECTED"

    def fail(self, tid):
        t = self.tasks[tid]
        if t["status"] != "Awarded" or self.now < t["spec"]["deliver_end_ms"]:
            return "WRONG_PHASE"
        if not self.live(t["grant"]):
            return self._void(t, "WINNER_NOT_LIVE")
        before = self._commitment(t)
        self._release_bid(t, t["winner"], to=self.vault)
        t["status"], t["note"] = "Failed", "nothing was delivered by deliver_end"
        self._settle(t, before)
        return "FAILED"

    def void(self, tid):
        t = self.tasks[tid]
        if t["status"] not in ("Open", "Awarding", "Awarded", "Delivered"):
            return "WRONG_PHASE"
        target = t["grant"] or t["spec"]["grant_id"]
        if self.live(target):
            return "STILL_LIVE"
        return self._void(t, "WINNER_NOT_LIVE" if t["grant"] else "POSTER_NOT_LIVE")

    def claim(self, tid):
        """Returns (code, paid now)."""
        t = self.tasks[tid]
        if t["grant"] is None:
            return "NO_GRANT", 0
        g = self.view(t["grant"])
        before = self._commitment(t)
        t["drawn"] = min(g["spent"], t["price"])
        fresh = g["direct_spent"] - self.seen.get(t["grant"], 0)
        if fresh > 0:
            self.seen[t["grant"]] = g["direct_spent"]
            t["held"] += fresh
            self.held_total += fresh
        if t["status"] == "Delivered" and self.now >= t["spec"]["review_end_ms"]:
            self._release_bid(t, t["winner"])
            t["status"], t["note"] = "Accepted", "accepted by silence"
            self._stat(t["spec"]["grant_id"])["accepted"] += 1
        pay = 0
        if t["status"] in ("Accepted", "Paid"):
            pay = min(t["held"], t["price"] - t["paid"])
            if pay:
                t["held"] -= pay
                self.held_total -= pay
                t["paid"] += pay
                self._credit(t["bids"][t["winner"]]["worker"], pay)
            if t["status"] == "Accepted" and (t["drawn"] == t["price"] or not g["live"]):
                t["status"] = "Paid"
        self._settle(t, before)
        return "CLAIMED", pay

    def refund(self, tid):
        t = self.tasks[tid]
        if t["status"] not in FINAL:
            return "WRONG_PHASE"
        if t["held"] == 0:
            return "NOTHING_HELD"
        self.held_total -= t["held"]
        t["refunded"] += t["held"]
        self._credit(self.vault, t["held"])
        t["held"] = 0
        return "REFUNDED"

    def payout(self, account):
        """Returns the amount sent."""
        amount = self.balances.pop(account, 0)
        self.received[account] = self.received.get(account, 0) + amount
        return amount

    def task_view(self, tid):
        t = self.tasks[tid]
        return {
            "id": tid, "status": t["status"], "note": t["note"], "price": t["price"], "paid": t["paid"], "held": t["held"],
            "refunded": t["refunded"], "grant": t["grant"], "poster": t["spec"]["grant_id"], "max_budget": t["spec"]["max_budget"],
            "winner": t["bids"][t["winner"]]["worker"] if t["winner"] is not None else None,
            "bids": [{"worker": b["worker"], "price": b["price"]} for b in t["bids"]],
        }
