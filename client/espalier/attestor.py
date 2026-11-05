"""The recipient check as a service: gathers signals, decides, and signs a ticket on `allow`.

It runs as its own process with its own key, because that is the whole point of a ticket: the agent
never holds this key, so an agent that skips its wrapper has no ticket and its spend goes to a human.
Tickets last a few minutes, since the code of a recipient can change right after this check, and the
contract cannot see that. The contract only checks that someone it trusts looked recently."""
import hashlib
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from . import near, policy as policy_mod, signals as sig


def clearance_message(vault, grant, recipient, max_amount, expires_at_ms, code_hash):
    """Must match `clearance_message` in contract/src/lib.rs byte for byte."""
    return (
        b"espalier-clearance-v1"
        + near.bstr(vault)
        + near.u64(grant)
        + near.bstr(recipient)
        + near.u128(max_amount)
        + near.u64(expires_at_ms)
        + code_hash
    )


class Attestor:
    def __init__(self, rpc, archival, vault, key, policy=None, ttl_secs=300, first_block=1, cache_secs=0):
        self.rpc = rpc
        self.archival = archival
        self.vault = vault
        self.key = key
        self.policy = policy or policy_mod.Policy()
        self.ttl_secs = ttl_secs
        self.first_block = first_block
        self.cache_secs = cache_secs
        self._cache = {}

    def _signals(self, recipient):
        hit = self._cache.get(recipient)
        if hit and time.time() - hit[0] < self.cache_secs:
            return dict(hit[1])
        fresh = sig.gather(self.rpc, self.archival, recipient, self.first_block)
        self._cache[recipient] = (time.time(), fresh)
        return dict(fresh)

    def check(self, grant, recipient, amount):
        info = self.rpc.view(self.vault, "vault_info")
        lineage = self.rpc.view(self.vault, "get_lineage", {"grant_id": grant})
        s = self._signals(recipient)
        if s.get("exists"):
            s["history"] = sig.history(self.rpc, self.vault, grant, lineage[-1], recipient)
            s["denylist"] = None
            if info.get("screen"):
                try:
                    s["denylist"] = sig.denylist(self.rpc, info["screen"], recipient)
                except near.RpcError:
                    pass
        out = policy_mod.decide(s, amount, self.policy)
        out.update({"recipient": recipient, "grant": grant, "amount": str(amount), "signals": s})
        if out["verdict"] == "allow":
            out["ticket"] = self.sign_ticket(grant, recipient, amount, s.get("code_hash", sig.ZERO_HASH), info["now_ms"])
        return out

    def sign_ticket(self, grant, recipient, amount, code_hash_b58, now_ms):
        expires = now_ms + self.ttl_secs * 1000
        if code_hash_b58.startswith("global:"):
            code = hashlib.sha256(code_hash_b58.encode()).digest()
        else:
            code = near.b58decode(code_hash_b58, 32)
        message = clearance_message(self.vault, grant, recipient, amount, expires, code)
        return {
            "max_amount": str(amount),
            "expires_at_ms": expires,
            "code_hash": code.hex(),
            "signature": self.key.sign(message).hex(),
        }


class _Handler(BaseHTTPRequestHandler):
    attestor = None

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        q = json.loads(self.rfile.read(length))
        try:
            body = self.attestor.check(int(q["grant"]), q["recipient"], int(q["amount"]))
            code = 200
        except Exception as e:  # The caller must see a failure, and treat it as "no ticket".
            body = {"verdict": "warn", "reasons": [{"level": "warn", "code": "SIGNAL_MISSING", "detail": str(e)[:200]}]}
            code = 503
        raw = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def log_message(self, *args):
        pass


def serve(attestor, port=0):
    """Starts the HTTP endpoint on localhost and returns (server, url)."""
    handler = type("H", (_Handler,), {"attestor": attestor})
    server = ThreadingHTTPServer(("127.0.0.1", port), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_address[1]}"


def evidence_hash(blob):
    return hashlib.sha256(blob).hexdigest()
