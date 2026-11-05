"""Sets up a vault, a registry, reporters and an attestor on a node, for the demos and the trials."""
import json
import os

from . import attestor as att
from . import near, policy as policy_mod, wrapper

NEAR = 10**24
MILLI = near.MILLI
RES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "res")


def wasm(name):
    return open(os.path.join(RES, f"{name}.wasm"), "rb").read()


def must(outcome, what):
    if not outcome.ok:
        raise RuntimeError(f"{what} failed: {outcome.failure()}")
    return outcome


class Owner:
    """The human. Holds the owner key, which approves spends and changes settings."""

    def __init__(self, signer, vault, decide=None):
        self.signer = signer
        self.vault = vault
        self.decide = decide or (lambda request: False)
        self.asked = []

    def review(self, request):
        yes = bool(self.decide(request))
        self.asked.append({"request": request, "answer": yes})
        return yes

    def approve(self, approval_id, spend_hash):
        r = self.signer.call(self.vault, "approve", {"approval_id": approval_id, "spend_hash": spend_hash}, tgas=50)
        return r.hash


def terms(now_ms, cap, per_tx, **over):
    t = {
        "asset": "Native",
        "cap": str(cap),
        "per_tx": str(per_tx),
        "window_cap": str(cap),
        "window_secs": 3600,
        "approval_threshold": str(per_tx),
        "recipients": None,
        "methods": ["request_spend", "execute_approved", "delegate", "revoke"],
        "expires_at_ms": now_ms + 86_400_000,
        "max_depth": 3,
        "max_denials": 1000,
        "key_allowance": str(500 * MILLI),
        "gas_budget": str(5000 * MILLI),
        "node_budget": 20,
        "clearance_required": True,
    }
    t.update(over)
    return t


class World:
    """Everything except the agents. `base` is an account that can create subaccounts."""

    def __init__(self, rpc, base, tag="", vault_near=40, policy=None, first_block=1, archival=None):
        self.rpc = rpc
        self.base = base
        self.archival = archival or rpc
        self.vault_id = f"vault{tag}.{base.account}"
        self.registry_id = f"registry{tag}.{base.account}"
        self.policy = policy or policy_mod.Policy()
        self.first_block = first_block
        self.tag = tag
        self.vault_near = vault_near

    def account(self, name, balance_near=2, key=None):
        key = key or near.Key.generate()
        account = name if "." in name else f"{name}.{self.base.account}"
        must(self.base.create_subaccount(account, int(balance_near * NEAR), key), f"create {account}")
        return near.Signer(self.rpc, account, key)

    def build(self, reporters=3, quorum=2):
        self.owner = self.account(f"owner{self.tag}", 10)
        vault = self.account(self.vault_id, self.vault_near)
        must(vault.deploy(wasm("espalier_vault")), "deploy vault")
        must(vault.call(self.vault_id, "new", {"owner": self.owner.account, "gas_floor": "0", "approval_ttl_secs": 3600}, tgas=30), "init vault")
        reg = self.account(self.registry_id, 5)
        must(reg.deploy(wasm("espalier_registry")), "deploy registry")
        must(reg.call(self.registry_id, "new", {}, tgas=30), "init registry")
        self.reporters = [self.account(f"reporter{i}{self.tag}", 1) for i in range(reporters)]
        self.attestor_key = near.Key.generate()
        must(self.owner.call(self.vault_id, "set_attestor", {"attestor": self.attestor_key.public_str}, tgas=30), "set attestor")
        must(
            self.owner.call(
                self.vault_id,
                "set_screen",
                {"screen": {"registry": self.registry_id, "reporters": [r.account for r in self.reporters], "quorum": quorum}},
                tgas=30,
            ),
            "set screen",
        )
        self.attestor = att.Attestor(self.rpc, self.archival, self.vault_id, self.attestor_key, self.policy, first_block=self.first_block)
        self.server, self.attestor_url = att.serve(self.attestor)
        return self

    def now_ms(self):
        return self.rpc.view(self.vault_id, "vault_info")["now_ms"]

    def human(self, decide=None):
        return Owner(self.owner, self.vault_id, decide)

    def root_agent(self, cap, per_tx, human=None, log=None, **over):
        key = near.Key.generate()
        r = self.owner.call(self.vault_id, "create_root", {"pk": key.public_str, "terms": terms(self.now_ms(), cap, per_tx, **over)}, tgas=100)
        must(r, "create root")
        grant = r.value["new_grant"]
        return wrapper.Agent(self.rpc, self.vault_id, key, grant, wrapper.http_checker(self.attestor_url), human, log)

    def worker(self, grant, key, human=None, log=None):
        return wrapper.Agent(self.rpc, self.vault_id, key, grant, wrapper.http_checker(self.attestor_url), human, log)

    def report(self, reporter, subject, evidence_blob, ttl_secs=86_400):
        digest = att.evidence_hash(evidence_blob)
        r = reporter.call(
            self.registry_id,
            "report",
            {"subject": subject, "category": "payment-diversion", "evidence_sha256": digest, "evidence_uri": f"evidence://{digest[:16]}", "ttl_secs": ttl_secs},
            tgas=30,
            deposit=10 * MILLI,
        )
        return must(r, "report")

    def balance(self, account):
        return int(self.rpc.view_account(account)["amount"])

    def close(self):
        if getattr(self, "server", None):
            self.server.shutdown()


def dump(path, doc):
    with open(path, "w", encoding="utf8") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
