#!/usr/bin/env python3
"""A subcontracting swarm with no LLM in it: one owner, one general contractor, three levels of
hiring, 25 agents with scripted strategies.

    python3 scripts/swarm-sim.py --offline                 # the model only, under a second
    NEAR_SANDBOX_BIN_PATH=/path/to/near-sandbox python3 scripts/swarm-sim.py --sandbox
    python3 scripts/swarm-sim.py --network testnet --base NAME.testnet   # keys in ~/.near-credentials

The same script drives the Python model (client/espalier/market_model.py) and a real node. On a
real node every action is a signed transaction, and the result is compared with what the model
predicted for the same script. Costs and budgets are invented inputs. What a run shows is that the
rules hold, not what work costs.

Strategies in the cast: truthful bidders, a bidder that never reveals, a lowballer that wins under its
cost and then cannot fund its subcontractors, three bidders that agree to bid the maximum, a winner
that never delivers, a winner that delivers bad work, a poster that never awards, and a contractor
that is hijacked, tries to pay an outsider, and is cut off by the general contractor together with
everyone it hired.
"""
import concurrent.futures
import hashlib
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "client"))

from espalier import market as mk  # noqa: E402
from espalier import market_model, near, snapshot  # noqa: E402

MILLI = near.MILLI
EVIDENCE = os.path.join(HERE, "..", "docs", "evidence")
BID_BOND = 5 * MILLI
POSTER_BOND = 10 * MILLI
KEY_ALLOWANCE = 250 * MILLI
ROOT_BUDGET = 1000

WORKERS = "ada bo cy dee eve fay gus hal ida jon kai lee max ned oz pam quin ray sam tom uma vic wes xia".split()
OUTSIDER = "mallory"

LINES = []


def say(text=""):
    print(text, flush=True)
    LINES.append(text)


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def sub_nodes(depth, fanout=3):
    """Node budget a winner needs to hire `fanout` subcontractors, each of whom may do the same."""
    return 0 if depth == 0 else fanout * (1 + sub_nodes(depth - 1, fanout))


def sub_gas(depth, fanout=3):
    """Gas budget for the same: each subcontractor's key allowance plus its own gas budget."""
    return 0 if depth == 0 else fanout * (KEY_ALLOWANCE + sub_gas(depth - 1, fanout))


class Holder:
    """A grant and the key that speaks for it. `name` is the agent's own account."""

    def __init__(self, name, grant, key):
        self.name, self.grant, self.key = name, grant, key


# ---- Two worlds with one interface -----------------------------------------------------------------


class ModelWorld:
    """The script on the Python model. Keys are plain strings and time is a counter."""

    kind = "offline"

    def __init__(self, unit_ms):
        self.unit = unit_ms
        self.m = market_model.Model(poster_bond=POSTER_BOND)
        self.m.now = 1_000_000
        self.market, self.vault = self.m.market, self.m.vault
        self.txs = []
        self.n = 0

    def setup(self):
        m = self.m
        code = m.set_roster("owner", WORKERS)
        assert code == "ROSTER_SET"
        key = self.new_key()
        gid = m.create_root(key, self.root_terms())
        return Holder("gc", gid, key)

    def root_terms(self):
        cap = ROOT_BUDGET * MILLI
        return {
            "cap": cap, "per_tx": cap, "window_cap": cap, "approval_threshold": cap, "recipients": [self.market],
            "methods": ["request_spend", "delegate", "revoke"], "expires_at_ms": self.m.now + 86_400_000, "max_depth": 4,
            "max_denials": 25, "key_allowance": KEY_ALLOWANCE, "gas_budget": 200 * KEY_ALLOWANCE, "node_budget": 60,
            "clearance_required": False,
        }

    def new_key(self):
        self.n += 1
        return f"key-{self.n}"

    def pub(self, key):
        return key

    def now(self):
        return self.m.now

    def wait(self, ts):
        self.m.now = max(self.m.now, ts)

    def many(self, calls):
        return [c() for c in calls]

    def post(self, holder, spec, signer=None, bond=POSTER_BOND):
        return self.m.post(spec, signer or holder.key, bond, holder.name)

    def commit(self, worker, task, price, key, salt, bond=BID_BOND):
        return self.m.commit(worker, task, (price, key, salt), bond)

    def reveal(self, worker, task, price, key, salt):
        return self.m.reveal(worker, task, price, key, salt)

    def close(self, task, by="gc"):
        return self.m.close(task)

    def delegate(self, holder, key, terms):
        return self.m.delegate(holder.grant, key, terms)

    def confirm(self, task, grant, by="gc"):
        return self.m.confirm(task, grant)

    def default(self, task):
        return self.m.default(task)

    def deliver(self, worker, task):
        return self.m.deliver(worker, task)

    def accept(self, holder, task, signer=None):
        return self.m.review(task, True, signer or holder.key)

    def reject(self, holder, task):
        return self.m.review(task, False, holder.key)

    def fail(self, task):
        return self.m.fail(task)

    def void(self, task):
        return self.m.void(task)

    def draw(self, holder, amount, to=None):
        return self.m.spend(holder.grant, to or self.market, amount)

    def claim(self, task):
        return self.m.claim(task)

    def refund(self, task):
        return self.m.refund(task)

    def payout(self, account):
        return self.m.payout(account)

    def revoke(self, by, grant):
        return self.m.revoke("owner" if by == "owner" else by.grant, grant)

    def grant(self, gid):
        return self.m.view(gid)

    def poster_view(self, holder):
        g = self.m.view(holder.grant)
        return {"window_secs": 3600, "expires_at_ms": g["expires_at_ms"], "max_denials": g["max_denials"]}

    def task(self, tid):
        return self.m.task_view(tid)

    def owed(self, account):
        return self.m.balances.get(account, 0)

    def account(self, name):
        return self.vault if name == "vault" else name

    def facts(self):
        return {"network": "none: Python model"}


class ChainWorld:
    """The script on a real node: every action is a signed transaction."""

    def __init__(self, rpc, base, unit_ms, kind, tag=""):
        self.rpc, self.base, self.unit, self.kind = rpc, base, unit_ms, kind
        # The script reads right after it writes, so it reads the newest state, not the final one.
        rpc.finality = "optimistic"
        self.vault = f"vault{tag}.{base.account}"
        self.market = f"market{tag}.{base.account}"
        self.tag = tag
        self.acct = {}
        self.txs = []
        self.pool = concurrent.futures.ThreadPoolExecutor(max_workers=8)
        # A public RPC is several nodes. A read sent right after a write can land on one that has not
        # seen the block yet, so on testnet a read waits this long after the last write.
        self.settle = 2.5 if kind == "testnet" else 0.0
        self.last_write = 0.0

    def wasm(self, name):
        return open(os.path.join(HERE, "..", "res", f"{name}.wasm"), "rb").read()

    def rec(self, out, what, code=None):
        """Every transaction of a run is recorded with its signer, so a verifier can look it up."""
        self.txs.append({"hash": out.hash, "signer": out.raw["transaction"]["signer_id"], "what": what, "code": code})
        self.last_write = time.time()

    def fresh(self):
        wait = self.last_write + self.settle - time.time()
        if wait > 0:
            time.sleep(wait)

    def must(self, out, what):
        self.rec(out, what, "ok" if out.ok else "FAILED")
        if not out.ok:
            raise RuntimeError(f"{what} failed: {out.failure()}")
        return out

    def sub(self, name, near_amount):
        key = near.Key.generate()
        account = f"{name}{self.tag}.{self.base.account}"
        self.must(self.base.create_subaccount(account, int(near_amount * 10**24), key), f"create {account}")
        if self.kind == "testnet":
            # Kept beside the base account's key, outside the repository, so the account can be
            # used or emptied again later.
            home = os.path.expanduser("~/.near-credentials/testnet")
            with open(os.path.join(home, f"{account}.json"), "w") as f:
                json.dump({"account_id": account, "public_key": key.public_str, "private_key": key.secret_str}, f)
        self.acct[name] = near.Signer(self.rpc, account, key)
        return self.acct[name]

    def setup(self):
        # On testnet the faucet gives 10 NEAR an account, so every balance is sized. Prepaid gas is
        # charged up front at about 0.001 NEAR per Tgas and mostly comes back, so an account that
        # posts needs about 0.1 NEAR free at the moment it sends.
        cheap = self.kind == "testnet"
        worker_near = 0.25 if cheap else 1.0
        vault = self.sub("vault", 5.6 if cheap else 30)
        market = self.sub("market", 3.8 if cheap else 8)
        self.vault, self.market = vault.account, market.account
        owner = self.sub("owner", 0.3 if cheap else 3)
        self.sub("gc", 0.6 if cheap else 3)
        for w in WORKERS + [OUTSIDER]:
            self.sub(w, worker_near)
        self.must(vault.deploy(self.wasm("espalier_vault")), "deploy vault")
        self.must(vault.call(self.vault, "new", {"owner": owner.account, "gas_floor": "0", "approval_ttl_secs": 3600}, tgas=30), "init vault")
        self.must(market.deploy(self.wasm("espalier_market")), "deploy market")
        self.must(
            market.call(self.market, "new", {"vaults": [self.vault], "poster_bond": str(POSTER_BOND), "min_window_ms": 1000, "draw_grace_ms": 1000}, tgas=30),
            "init market",
        )
        self.mk = mk.Market(self.rpc, self.market)
        out = owner.call(self.market, "set_roster", {"vault": self.vault, "workers": [self.acct[w].account for w in WORKERS]}, tgas=60)
        self.must(out, "set roster")
        assert mk.code_of(out) == "ROSTER_SET", mk.code_of(out)
        key = near.Key.generate()
        out = owner.call(self.vault, "create_root", {"pk": key.public_str, "terms": self.root_terms()}, tgas=100)
        self.must(out, "create root")
        self.signers = {}
        return self.holder("gc", out.value["new_grant"], key)

    def root_terms(self):
        cap = str(ROOT_BUDGET * MILLI)
        return {
            "asset": "Native", "cap": cap, "per_tx": cap, "window_cap": cap, "window_secs": 3600, "approval_threshold": cap,
            "recipients": [self.market], "methods": ["request_spend", "delegate", "revoke"], "expires_at_ms": self.now() + 86_400_000,
            "max_depth": 4, "max_denials": 25, "key_allowance": str(KEY_ALLOWANCE), "gas_budget": str(200 * KEY_ALLOWANCE),
            "node_budget": 60, "clearance_required": False,
        }

    def holder(self, name, grant, key):
        h = Holder(name, grant, key)
        self.signers[grant] = near.Signer(self.rpc, self.vault, key)
        return h

    def new_key(self):
        return near.Key.generate()

    def pub(self, key):
        return key.public_str

    def now(self):
        return self.rpc.view(self.vault, "vault_info")["now_ms"]

    def wait(self, ts):
        """Sleeps until chain time passes `ts`, asking the chain only now and then."""
        while True:
            left = ts - self.now()
            if left <= 0:
                return
            time.sleep(min(max(left / 1000, 0.4), 15))

    def many(self, calls):
        """Runs calls from different accounts side by side. Results come back in order."""
        return [f.result() for f in [self.pool.submit(c) for c in calls]]

    def keep(self, out, what):
        self.rec(out, what, mk.code_of(out))
        return out

    def post(self, holder, spec, signer=None, bond=POSTER_BOND):
        out = self.keep(self.mk.post(self.acct[holder.name], self.wire(spec), signer or holder.key, bond), f"post_task by {holder.name}")
        code = mk.code_of(out)
        return code, (out.value or {}).get("task") if out.ok else None

    @staticmethod
    def wire(spec):
        return {k: (str(v) if k in ("max_budget", "bid_bond", "sub_gas", "key_allowance") else v) for k, v in spec.items()}

    def commit(self, worker, task, price, key, salt, bond=BID_BOND):
        return mk.code_of(self.keep(self.mk.commit(self.acct[worker], task, price, key.public_str, salt, bond), f"commit_bid by {worker} on {task}"))

    def reveal(self, worker, task, price, key, salt):
        return mk.code_of(self.keep(self.mk.reveal(self.acct[worker], task, price, key.public_str, salt), f"reveal_bid by {worker} on {task}"))

    def close(self, task, by="gc"):
        out = self.keep(self.mk.close(self.acct[by], task), f"close_bidding {task}")
        v = out.value or {}
        return mk.code_of(out), int(v["amount"]) if v.get("amount") else None

    def delegate(self, holder, key, terms):
        t = dict(terms, asset="Native", window_secs=3600)
        for k in ("cap", "per_tx", "window_cap", "approval_threshold", "key_allowance", "gas_budget"):
            t[k] = str(t[k])
        out = self.signers[holder.grant].call(self.vault, "delegate", {"child_pk": key.public_str, "terms": t}, tgas=60)
        self.rec(out, f"vault.delegate by grant {holder.grant}", (out.value or {}).get("code"))
        v = out.value or {}
        return v.get("code"), v.get("new_grant")

    def confirm(self, task, grant, by="gc"):
        return mk.code_of(self.keep(self.mk.confirm(self.acct[by], task, grant), f"confirm_award {task}"))

    def default(self, task):
        return mk.code_of(self.keep(self.mk.default(self.acct["gc"], task), f"default_award {task}"))

    def deliver(self, worker, task):
        return mk.code_of(self.keep(self.mk.deliver(self.acct[worker], task, sha(f"result of task {task}")), f"deliver {task} by {worker}"))

    def accept(self, holder, task, signer=None):
        return mk.code_of(self.keep(self.mk.accept(self.acct[holder.name], task, signer or holder.key), f"accept {task}"))

    def reject(self, holder, task):
        return mk.code_of(self.keep(self.mk.reject(self.acct[holder.name], task, sha(f"why task {task} was rejected"), holder.key), f"reject {task}"))

    def fail(self, task):
        return mk.code_of(self.keep(self.mk.fail_delivery(self.acct["gc"], task), f"fail_delivery {task}"))

    def void(self, task):
        return mk.code_of(self.keep(self.mk.void(self.acct["gc"], task), f"void_task {task}"))

    def draw(self, holder, amount, to=None):
        try:
            out = self.signers[holder.grant].call(self.vault, "request_spend", {"recipient": to or self.market, "amount": str(amount), "memo": None}, tgas=60)
        except near.RpcError as e:
            return ("NO_KEY", None) if "NO_KEY" in str(e.data) else (str(e.data)[:60], None)
        v = out.value or {}
        self.rec(out, f"vault.request_spend by grant {holder.grant}", v.get("code"))
        return v.get("code"), v.get("blocking_grant")

    def claim(self, task):
        out = self.keep(self.mk.claim(self.acct["gc"], task), f"claim {task}")
        v = out.value or {}
        return mk.code_of(out), int(v.get("amount") or 0)

    def refund(self, task):
        return mk.code_of(self.keep(self.mk.refund(self.acct["gc"], task), f"refund {task}"))

    def payout(self, account):
        self.fresh()
        owed = self.mk.owed(account)
        if owed:
            self.keep(self.mk.payout(self.acct["gc"], account), f"payout {account}")
        return owed

    def revoke(self, by, grant):
        signer = self.acct["owner"] if by == "owner" else self.signers[by.grant]
        try:
            out = signer.call(self.vault, "revoke", {"grant_id": grant}, tgas=60)
        except near.RpcError as e:
            return "NO_KEY" if "NO_KEY" in str(e.data) else str(e.data)[:60]
        self.rec(out, f"vault.revoke {grant}", (out.value or {}).get("code"))
        return (out.value or {}).get("code")

    def grant(self, gid):
        self.fresh()
        g = self.rpc.view(self.vault, "get_grant", {"grant_id": gid})
        if g is None:
            return None
        for k in ("cap", "spent", "direct_spent", "spendable"):
            g[k] = int(g[k])
        return g

    def poster_view(self, holder):
        self.fresh()
        g = self.rpc.view(self.vault, "get_grant", {"grant_id": holder.grant})
        return {"window_secs": g["window_secs"], "expires_at_ms": g["expires_at_ms"], "max_denials": g["max_denials"]}

    def task(self, tid):
        self.fresh()
        t = self.mk.task(tid)
        return {
            "id": tid, "status": t["status"], "note": t["note"], "price": int(t["price"]), "paid": int(t["paid"]), "held": int(t["held"]),
            "refunded": int(t["refunded"]), "grant": t["grant"], "poster": t["spec"]["grant_id"], "max_budget": int(t["spec"]["max_budget"]),
            "winner": self.short(t["winner"]), "bids": [{"worker": self.short(b["worker"]), "price": int(b["price"]) if b["price"] else None} for b in t["bids"]],
        }

    def short(self, account):
        if not account:
            return None
        name = account.split(".")[0]
        return name[: len(name) - len(self.tag)] if self.tag else name

    def owed(self, account):
        return self.mk.owed(account)

    def account(self, name):
        return self.vault if name == "vault" else self.acct[name].account

    def facts(self):
        return {
            "network": self.kind, "vault": self.vault, "market": self.market,
            "vault_wasm_sha256": hashlib.sha256(self.wasm("espalier_vault")).hexdigest(),
            "market_wasm_sha256": hashlib.sha256(self.wasm("espalier_market")).hexdigest(),
            "accounts": {n: s.account for n, s in self.acct.items()},
        }


# ---- The script ------------------------------------------------------------------------------------


class Story:
    def __init__(self, w):
        self.w = w
        self.tasks = {}
        self.holders = {}
        self.checks = []
        self.nonce = 0

    def check(self, claim, ok, detail=""):
        self.checks.append({"claim": claim, "pass": bool(ok), "detail": str(detail)})
        say(f"    {'ok  ' if ok else 'FAIL'} {claim}" + (f"  [{detail}]" if detail and not ok else ""))

    def expect(self, claim, got, want):
        self.check(f"{claim}: {want}", got == want, f"got {got}")

    def spec(self, holder, name, budget, t, depth, min_bids=2):
        """`t` is (commit_end, reveal_end, award_end, deliver_end, review_end) in units after T0."""
        return {
            "vault": self.w.vault, "grant_id": holder.grant, "spec_hash": sha(f"brief: {name}"), "max_budget": budget * MILLI,
            "bid_bond": BID_BOND, "min_bids": min_bids,
            "commit_end_ms": self.t0 + t[0] * self.w.unit, "reveal_end_ms": self.t0 + t[1] * self.w.unit,
            "award_end_ms": self.t0 + t[2] * self.w.unit, "deliver_end_ms": self.t0 + t[3] * self.w.unit,
            "review_end_ms": self.t0 + t[4] * self.w.unit,
            "sub_depth": depth, "sub_nodes": sub_nodes(depth), "sub_gas": sub_gas(depth), "key_allowance": KEY_ALLOWANCE,
            "nonce": self.nonce,
        }

    def post_round(self, posts):
        """`posts` is a list of (label, holder, task name, budget, timetable, depth, min_bids, expected code).
        Each poster sends its own posts one after another, and different posters post side by side."""
        specs = []
        for _, holder, name, budget, t, depth, min_bids, _ in posts:
            self.nonce += 1
            specs.append(self.spec(holder, name, budget, t, depth, min_bids))
        results = self.grouped([lambda h=p[1], s=s: self.w.post(h, s) for p, s in zip(posts, specs)], [p[1].name for p in posts])
        for (label, holder, name, _, _, _, _, want), spec, (code, tid) in zip(posts, specs, results):
            if tid is not None:
                self.tasks[name] = {"id": tid, "spec": spec, "poster": holder, "bids": {}}
            self.expect(label, code, want)

    def at(self, units):
        self.w.wait(self.t0 + int(units * self.w.unit) + 1)

    def bid_round(self, plan, commit_at, silent=(), first=()):
        """`plan` maps task name -> {worker: price in mNEAR}. Commits, waits, reveals. Equal bids are
        ranked by who committed first, so a worker in `first` commits before everyone else."""
        w = self.w
        calls, who = [], []
        for name, bids in plan.items():
            for worker, price in bids.items():
                key, salt = w.new_key(), hashlib.sha256(f"{name}/{worker}".encode()).digest()
                self.tasks[name]["bids"][worker] = (price * MILLI, key, salt)
                calls.append(lambda n=name, wk=worker, p=price * MILLI, k=key, s=salt: w.commit(wk, self.tasks[n]["id"], p, k, s))
                who.append((name, worker))
        early = [i for i, x in enumerate(who) if x[1] in first]
        codes = [None] * len(calls)
        for i in early:
            codes[i] = calls[i]()
        # One account sends one transaction at a time, so the rest are grouped by worker.
        rest = [i for i in range(len(calls)) if i not in early]
        for i, code in zip(rest, self.grouped([calls[i] for i in rest], [who[i][1] for i in rest])):
            codes[i] = code
        bad = [f"{n}/{wk}: {c}" for (n, wk), c in zip(who, codes) if c != "ok"]
        self.check(f"{len(calls)} sealed bids committed", not bad, bad)
        self.at(commit_at)
        calls, who = [], []
        for name, bids in plan.items():
            for worker in bids:
                if worker in silent:
                    continue
                p, k, s = self.tasks[name]["bids"][worker]
                calls.append(lambda n=name, wk=worker, p=p, k=k, s=s: w.reveal(wk, self.tasks[n]["id"], p, k, s))
                who.append((name, worker))
        codes = self.grouped(calls, [x[1] for x in who])
        bad = [f"{n}/{wk}: {c}" for (n, wk), c in zip(who, codes) if c != "REVEALED"]
        self.check(f"{len(calls)} bids revealed" + (f", {', '.join(silent)} stayed silent" if silent else ""), not bad, bad)

    def grouped(self, calls, senders):
        by = {}
        for i, s in enumerate(senders):
            by.setdefault(s, []).append(i)
        out = [None] * len(calls)

        def run(idx):
            return [(i, calls[i]()) for i in idx]

        for chunk in self.w.many([lambda idx=idx: run(idx) for idx in by.values()]):
            for i, code in chunk:
                out[i] = code
        return out

    def terms(self, name, price, **over):
        t = self.tasks[name]
        base = mk.award_terms(t["spec"], price, self.w.poster_view(t["poster"]), self.w.market)
        base.update(over)
        return base

    def settle_round(self, rows, notes=None):
        """`rows` is a list of (task name, expected winner, expected price in mNEAR, award?). Bidding is
        closed, and the poster creates the winner's grant in the vault and has the market confirm it.
        Each poster handles its own tasks one after another, and different posters work side by side."""
        w = self.w

        def one(name, winner, award):
            t = self.tasks[name]
            carrier = t["poster"].name
            code, got = w.close(t["id"], by=carrier)
            res = {"close": (code, w.task(t["id"])["winner"], got)}
            if award and code == "WINNER":
                key = t["bids"][winner][1]
                dcode, gid = w.delegate(t["poster"], key, self.terms(name, got))
                res["award"] = (dcode, w.confirm(t["id"], gid, by=carrier) if gid else None, gid, key)
            return res

        results = self.grouped([lambda r=r: one(r[0], r[1], r[3]) for r in rows], [self.tasks[r[0]]["poster"].name for r in rows])
        out = []
        for (name, winner, price, award), res in zip(rows, results):
            code, who, got = res["close"]
            self.check(f"{name}: {winner} wins at {price} mNEAR", code == "WINNER" and who == winner and got == price * MILLI, f"{code} {who} {got}")
            if notes and name in notes:
                say("    " + notes[name])
            if award:
                dcode, ccode, gid, key = res.get("award", (None, None, None, None))
                poster = self.tasks[name]["poster"].name
                self.check(f"{name}: {poster} creates a grant for {winner} with cap {price} mNEAR, and the market confirms it is the award", dcode == "GRANTED" and ccode == "AWARDED", f"{dcode} {ccode}")
                self.holders[winner] = w.holder(winner, gid, key) if hasattr(w, "holder") else Holder(winner, gid, key)
                out.append(self.holders[winner])
        return out

    def paid_to(self, name, worker, amount):
        t = self.tasks[name]
        code, _ = self.w.draw(self.holders[worker], amount * MILLI)
        _, got = self.w.claim(t["id"])
        self.check(f"{name}: {worker} draws {amount} mNEAR from its own grant and the market forwards it", code == "SETTLED" and got == amount * MILLI, f"{code} {got}")

    def run(self):
        w = self.w
        gc = w.setup()
        self.holders["gc"] = gc
        self.t0 = w.now() + int(0.15 * w.unit)
        say(f"general contractor: grant {gc.grant}, budget {ROOT_BUDGET} mNEAR, can pay only {w.market}")
        say(f"{len(WORKERS)} workers on the owner's roster. One window is {w.unit / 1000:.0f} s.")

        say("\n1. The general contractor posts three tasks. Sealed bids.")
        L1 = (1, 2, 3, 13, 14)
        self.post_round([(f"post {name}, up to {budget} mNEAR", gc, name, budget, L1, 2, 2, "POSTED") for name, budget in (("api", 300), ("app", 300), ("data", 250))])
        self.bid_round(
            {"api": {"ada": 200, "bo": 230, "cy": 260, "dee": 150}, "app": {"eve": 150, "fay": 210, "gus": 250}, "data": {"hal": 180, "ida": 200, "jon": 220}},
            commit_at=1,
            silent=("dee",),
        )
        self.at(2)
        ada, eve, hal = self.settle_round(
            [("api", "ada", 230, True), ("app", "eve", 210, True), ("data", "hal", 200, True)],
            notes={
                "api": "dee bid lowest on api and never revealed: its bond goes to the vault, and it does not win",
                "app": "eve's real cost is 240 and she bid 150 to win: she is paid 210, the next bid, and is under water",
            },
        )
        self.expect("nothing has left the vault", w.grant(gc.grant)["spent"], 0)

        say("\n2. Each winner posts tasks of its own, from the grant it just won.")
        self.at(3)
        L2 = (4, 5, 6, 11, 12)
        self.post_round([
            ("ada posts auth, up to 80", ada, "auth", 80, L2, 1, 2, "POSTED"),
            ("ada posts billing, up to 70", ada, "billing", 70, L2, 1, 2, "POSTED"),
            ("eve posts ui, up to 120", eve, "ui", 120, L2, 1, 2, "POSTED"),
            ("eve posts payments, up to 120: she has 210 and already promised 120", eve, "payments", 120, L2, 1, 2, "OVERCOMMITTED"),
            ("eve posts payments again, up to 90", eve, "payments", 90, L2, 1, 2, "POSTED"),
            ("eve posts a third task, up to 40: nothing is left to promise", eve, "search", 40, L2, 1, 2, "OVERCOMMITTED"),
            ("hal posts ingest, up to 80", hal, "ingest", 80, L2, 1, 2, "POSTED"),
            ("hal posts clean, up to 60", hal, "clean", 60, L2, 1, 2, "POSTED"),
        ])
        self.bid_round(
            {
                "auth": {"kai": 60, "lee": 70, "max": 75},
                "billing": {"ned": 70, "oz": 70, "pam": 70},
                "ui": {"quin": 90, "ray": 100},
                "payments": {"sam": 85, "tom": 88},
                "ingest": {"uma": 60, "vic": 70},
                "clean": {"wes": 40, "xia": 50},
            },
            commit_at=4,
            first=("ned",),
        )
        self.at(5)
        kai, ned, quin, sam, uma, wes = self.settle_round(
            [("auth", "kai", 70, True), ("billing", "ned", 70, True), ("ui", "quin", 100, True), ("payments", "sam", 88, True), ("ingest", "uma", 70, True), ("clean", "wes", 50, True)],
            notes={"billing": "ned, oz and pam agreed to bid the maximum on billing: the price is the budget, 70, and no more"},
        )

        say("\n3. The third level.")
        self.at(6)
        L3 = (7, 8, 9, 11, 12)
        self.post_round([
            ("kai posts tests, up to 25", kai, "tests", 25, L3, 0, 2, "POSTED"),
            ("quin posts icons, up to 30, one bid is enough", quin, "icons", 30, L3, 0, 1, "POSTED"),
            ("uma posts parser, up to 30", uma, "parser", 30, L3, 0, 2, "POSTED"),
            ("ned posts report, up to 20, one bid is enough", ned, "report", 20, L3, 0, 1, "POSTED"),
        ])
        code = w.commit(OUTSIDER, self.tasks["parser"]["id"], MILLI, w.new_key(), b"\0" * 32)
        self.expect(f"{OUTSIDER}, an account off the owner's roster, tries to bid on parser", code, "NOT_ON_ROSTER")
        self.bid_round({"tests": {"bo": 20, "cy": 15}, "icons": {"fay": 22}, "parser": {"gus": 25, "ida": 20}, "report": {"jon": 12}}, commit_at=7)
        self.at(8)
        cy, fay, ida = self.settle_round(
            [("tests", "cy", 20, True), ("icons", "fay", 30, True), ("parser", "ida", 25, True), ("report", "jon", 20, False)],
            notes={"icons": "fay was the only bidder on icons and is paid the budget, 30", "report": "ned does not create jon's grant"},
        )
        tree = w.grant(ida.grant)
        lineage = []
        g = tree
        while g:
            lineage.append(g["id"])
            g = w.grant(g["parent"]) if g["parent"] else None
        self.expect("ida's grant sits four levels under the owner: ida < uma < hal < gc", lineage, [ida.grant, uma.grant, hal.grant, gc.grant])

        say("\n4. hal is hijacked by an instruction hidden in a document.")
        outsider = w.account(OUTSIDER)
        code, _ = w.draw(hal, 50 * MILLI, to=outsider)
        self.expect(f"hal pays {OUTSIDER} from its grant", code, "RECIPIENT_NOT_ALLOWED")
        wide = self.terms("data", 10 * MILLI, recipients=[outsider], max_depth=0, node_budget=0, gas_budget=0, methods=["request_spend"])
        code, _ = w.delegate(hal, w.new_key(), wide)
        self.expect(f"hal creates a grant that can pay {OUTSIDER}", code, "ATT_RECIPIENTS")
        code, _ = w.draw(hal, 100 * MILLI)
        self.expect("hal draws 100 into the market before any of its work is accepted", code, "SETTLED")
        _, paid = w.claim(self.tasks["data"]["id"])
        self.expect("the market pays hal none of it", paid, 0)
        denied = w.grant(hal.grant)["denied"]
        say(f"    the general contractor reads hal's grant from the vault: {denied} refused attempts")
        self.expect("the general contractor revokes hal: one transaction", w.revoke(gc, hal.grant), "REVOKED")
        live = [w.grant(h.grant)["live"] for h in (hal, uma, wes, ida)]
        self.expect("hal, uma, wes and ida are all cut off", live, [False] * 4)
        for name in ("data", "ingest", "clean", "parser"):
            self.expect(f"anyone voids {name}", w.void(self.tasks[name]["id"]), "VOID")
        self.expect("the 100 hal drew goes back to the vault", w.refund(self.tasks["data"]["id"]), "REFUNDED")
        code, _ = w.draw(ida, MILLI)
        self.expect("ida, three levels down, can no longer draw", code, "NO_KEY")

        say("\n5. Work is delivered and reviewed, from the bottom up.")
        self.at(9)
        self.expect("ned created no grant for jon: the poster defaulted, and jon gets ned's bond", w.default(self.tasks["report"]["id"]), "DEFAULTED")
        self.expect("cy delivers tests", w.deliver("cy", self.tasks["tests"]["id"]), "ok")
        self.expect("kai rejects it: the tests do not run", w.reject(kai, self.tasks["tests"]["id"]), "REJECTED")
        self.expect("and revokes cy's grant", w.revoke(kai, cy.grant), "REVOKED")
        self.expect("fay delivers icons", w.deliver("fay", self.tasks["icons"]["id"]), "ok")
        self.expect("quin accepts", w.accept(quin, self.tasks["icons"]["id"]), "ACCEPTED")
        self.paid_to("icons", "fay", 30)
        self.expect("kai delivers auth", w.deliver("kai", self.tasks["auth"]["id"]), "ok")
        self.expect("kai accepts its own work, signing with its own key", w.accept(kai, self.tasks["auth"]["id"]), "BAD_SIGNATURE")
        self.expect("ada accepts", w.accept(ada, self.tasks["auth"]["id"]), "ACCEPTED")
        self.paid_to("auth", "kai", 70)
        self.expect("quin delivers ui", w.deliver("quin", self.tasks["ui"]["id"]), "ok")
        self.expect("eve accepts", w.accept(eve, self.tasks["ui"]["id"]), "ACCEPTED")
        code, blocking = w.draw(quin, 100 * MILLI)
        self.expect("quin draws its whole price of 100: 30 of it already went to fay", code, "CAP")
        self.paid_to("ui", "quin", 70)

        say("\n6. Deadlines.")
        self.at(11)
        self.expect("ned delivered nothing on billing: its bond goes to the vault", w.fail(self.tasks["billing"]["id"]), "FAILED")
        self.expect("ada revokes ned", w.revoke(ada, ned.grant), "REVOKED")
        self.expect("sam delivered nothing on payments either", w.fail(self.tasks["payments"]["id"]), "FAILED")
        self.expect("ada delivers api", w.deliver("ada", self.tasks["api"]["id"]), "ok")
        self.expect("the general contractor accepts", w.accept(gc, self.tasks["api"]["id"]), "ACCEPTED")
        code, _ = w.draw(ada, 230 * MILLI)
        self.expect("ada draws her whole price of 230: 70 of it already went to kai", code, "CAP")
        self.paid_to("api", "ada", 160)
        self.at(13)
        self.expect("eve delivered nothing on app: her bond goes to the vault", w.fail(self.tasks["app"]["id"]), "FAILED")
        self.expect("the general contractor revokes eve", w.revoke(gc, eve.grant), "REVOKED")
        self.expect("sam's grant is cut off with her", w.grant(sam.grant)["live"], False)

        say("\n7. Everyone is paid what the market owes them.")
        got = {}
        for name in WORKERS + ["gc", "vault"]:
            got[name] = w.payout(w.account(name)) // MILLI
        b, p = BID_BOND // MILLI, POSTER_BOND // MILLI
        want = {n: 0 for n in WORKERS + ["gc", "vault"]}
        for n in WORKERS:
            want[n] = b                                 # every bid bond comes back, except:
        for n in ("dee", "ned", "sam", "eve"):
            want[n] -= b                                # never revealed, or delivered nothing
        want.update(bo=2 * b, cy=2 * b, fay=2 * b + 30, ida=2 * b, jon=2 * b + p, gus=2 * b)  # bid twice
        want["ada"] += 160 + 2 * p                      # margin, and the bonds for two posts
        want["kai"] += 70 + p
        want["quin"] += 70 + p
        want["eve"] += 2 * p
        want["hal"] += 2 * p
        want["uma"] += p
        want["gc"] = 3 * p
        want["vault"] = 4 * b + 100                     # four forfeited bonds, and hal's 100
        self.expect("payouts in mNEAR", got, want)
        root = w.grant(gc.grant)
        self.expect("the tree was charged 160 + 70 for api, 70 + 30 for app, and hal's 100", root["spent"] // MILLI, 430)
        self.expect("the root can still spend the rest of its 1000", root["spendable"] // MILLI, 570)
        return self.summary()

    def summary(self):
        w = self.w
        tasks = {}
        for name, t in self.tasks.items():
            v = w.task(t["id"])
            tasks[name] = {
                "id": v["id"], "poster": t["poster"].name, "status": v["status"], "note": v["note"], "winner": v["winner"],
                "price_mnear": v["price"] // MILLI, "paid_mnear": v["paid"] // MILLI, "refunded_mnear": v["refunded"] // MILLI,
                "bids_mnear": {b["worker"]: (b["price"] // MILLI if b["price"] else None) for b in v["bids"]},
            }
        grants = {}
        for name, h in self.holders.items():
            g = w.grant(h.grant)
            grants[name] = {"grant": h.grant, "parent": g["parent"], "cap_mnear": g["cap"] // MILLI, "spent_mnear": g["spent"] // MILLI, "live": g["live"]}
        return {"tasks": tasks, "grants": grants, "checks": self.checks, "passed": all(c["pass"] for c in self.checks)}


def comparable(summary):
    """What a real run and the model must agree on, by name. Ids are left out: posters work side by
    side on a real node, so the order in which tasks and grants are numbered can differ."""
    owner = {g["grant"]: name for name, g in summary["grants"].items()}
    tasks = {name: {k: v for k, v in t.items() if k != "id"} for name, t in summary["tasks"].items()}
    grants = {name: {"under": owner.get(g["parent"]), "cap_mnear": g["cap_mnear"], "spent_mnear": g["spent_mnear"], "live": g["live"]} for name, g in summary["grants"].items()}
    return {"tasks": tasks, "grants": grants}


def main():
    args = sys.argv[1:]

    def opt(name, default=None):
        return args[args.index(name) + 1] if name in args else default

    if "--offline" in args or not args:
        started = time.time()
        summary = Story(ModelWorld(unit_ms=60_000)).run()
        say(f"\n{sum(c['pass'] for c in summary['checks'])}/{len(summary['checks'])} checks passed, offline, {time.time() - started:.2f} s")
        if "--write" in args:
            write("offline", summary, {"network": "none: Python model"}, [])
        return 0 if summary["passed"] else 1

    predicted = None
    lines = list(LINES)
    quiet = sys.stdout
    sys.stdout = open(os.devnull, "w")
    try:
        predicted = Story(ModelWorld(unit_ms=60_000)).run()
    finally:
        sys.stdout = quiet
        LINES[:] = lines

    if "--sandbox" in args:
        from espalier import sandbox

        with sandbox.Sandbox(port=int(opt("--port", "3081"))) as sb:
            w = ChainWorld(sb.rpc, sb.root, unit_ms=int(opt("--unit", "45")) * 1000, kind="sandbox")
            return finish(w, predicted, "sandbox")
    if opt("--network") == "testnet":
        base = opt("--base")
        home = os.path.expanduser("~/.near-credentials/testnet")
        data = json.load(open(os.path.join(home, f"{base}.json")))
        rpc = near.Rpc(opt("--rpc", "https://test.rpc.fastnear.com"))
        signer = near.Signer(rpc, base, near.Key.from_string(data.get("private_key") or data["secret_key"]))
        w = ChainWorld(rpc, signer, unit_ms=int(opt("--unit", "150")) * 1000, kind="testnet", tag=opt("--tag", ""))
        return finish(w, predicted, "testnet")
    print(__doc__)
    return 2


def finish(w, predicted, tag):
    started = time.time()
    story = Story(w)
    try:
        summary = story.run()
    finally:
        w.pool.shutdown(wait=False)
    same = comparable(summary) == comparable(predicted)
    story.check("the chain ended exactly where the Python model predicted: every task, winner, price, payment and grant", same)
    summary["checks"] = story.checks
    summary["passed"] = all(c["pass"] for c in story.checks)
    summary["matches_model"] = same
    say(f"\n{sum(c['pass'] for c in summary['checks'])}/{len(summary['checks'])} checks passed on {tag}, {len(w.txs)} transactions, {time.time() - started:.0f} s")
    write(tag, summary, w.facts(), w.txs)
    # What the live page shows, read through the same views, for when no RPC answers.
    snap = snapshot.take(w.rpc, w.market, w.vault, {s.account: n for n, s in w.acct.items()})
    snap["network"] = tag
    args = sys.argv[1:]
    default = os.path.join(HERE, "..", "site", "snapshot.js") if tag == "testnet" else os.path.join(HERE, "..", ".logs", f"snapshot-{tag}.js")
    snapshot.write_js(args[args.index("--snapshot") + 1] if "--snapshot" in args else default, snap)
    return 0 if summary["passed"] else 1


def write(tag, summary, facts, txs):
    doc = dict(facts, agents=len(WORKERS) + 1, agents_are_scripted=True, transactions=len(txs), **summary)
    doc["txs"] = txs
    with open(os.path.join(EVIDENCE, f"swarm-sim-{tag}.json"), "w", encoding="utf8", newline="\n") as f:
        f.write(json.dumps(doc, indent=1) + "\n")
    with open(os.path.join(EVIDENCE, f"swarm-sim-{tag}.log"), "w", encoding="utf8", newline="\n") as f:
        f.write("\n".join(LINES) + "\n")


if __name__ == "__main__":
    sys.exit(main())
