"""A small NEAR client on the standard library: JSON-RPC, borsh transactions, ed25519 signing.
It holds one key at a time and has no idea what a full-access key is for: it signs what it is told,
and the protocol decides what that key may do."""
import base64
import hashlib
import json
import os
import struct
import time
import urllib.error
import urllib.request

from . import ed25519

ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
TGAS = 10**12
MILLI = 10**21


def b58encode(b):
    n = int.from_bytes(b, "big")
    out = ""
    while n:
        n, r = divmod(n, 58)
        out = ALPHABET[r] + out
    return "1" * (len(b) - len(b.lstrip(b"\0"))) + out


def b58decode(s, size=None):
    n = 0
    for c in s:
        n = n * 58 + ALPHABET.index(c)
    pad = len(s) - len(s.lstrip("1"))
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big") if n else b""
    raw = b"\0" * pad + raw
    if size is not None and len(raw) != size:
        raise ValueError(f"expected {size} bytes, got {len(raw)}")
    return raw


# ---- Borsh, as much as a transaction needs -------------------------------------------------------

def u8(n):
    return struct.pack("<B", n)


def u32(n):
    return struct.pack("<I", n)


def u64(n):
    return struct.pack("<Q", n)


def u128(n):
    return n.to_bytes(16, "little")


def bstr(s):
    b = s.encode() if isinstance(s, str) else s
    return u32(len(b)) + b


def _pk(raw):
    return u8(0) + raw


def act_create_account():
    return u8(0)


def act_deploy(code):
    return u8(1) + bstr(code)


def act_call(method, args, gas, deposit):
    payload = args if isinstance(args, bytes) else json.dumps(args).encode()
    return u8(2) + bstr(method) + bstr(payload) + u64(gas) + u128(deposit)


def act_transfer(amount):
    return u8(3) + u128(amount)


def act_add_full_key(pk):
    return u8(5) + _pk(pk) + u64(0) + u8(1)


def act_delete_account(beneficiary):
    return u8(7) + bstr(beneficiary)


class Key:
    """An ed25519 keypair. `generate` makes a fresh one; only the public half ever leaves here."""

    def __init__(self, seed):
        self.seed = seed
        self.public = ed25519.public_key(seed)

    @classmethod
    def generate(cls):
        return cls(os.urandom(32))

    @classmethod
    def from_string(cls, text):
        raw = b58decode(text.split(":", 1)[1])
        return cls(raw[:32])

    @property
    def public_str(self):
        return "ed25519:" + b58encode(self.public)

    @property
    def secret_str(self):
        return "ed25519:" + b58encode(self.seed + self.public)

    def sign(self, message):
        return ed25519.sign(self.seed, message)

    def __repr__(self):
        return f"Key({self.public_str})"


class RpcError(Exception):
    def __init__(self, data):
        super().__init__(str(data)[:500])
        self.data = data


class Rpc:
    def __init__(self, url, retries=6):
        self.url = url
        self.retries = retries
        self.calls = 0

    def raw(self, method, params, timeout=40):
        body = json.dumps({"jsonrpc": "2.0", "id": "espalier", "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url, body, {"Content-Type": "application/json", "User-Agent": "espalier-client"})
        for attempt in range(self.retries):
            self.calls += 1
            try:
                with urllib.request.urlopen(req, timeout=timeout) as r:
                    return json.load(r)
            except urllib.error.HTTPError as e:
                body = e.read()
                if e.code not in (408, 429, 500, 502, 503, 504):
                    # Some providers send RPC errors as HTTP 4xx with the usual JSON body.
                    try:
                        return json.loads(body)
                    except ValueError:
                        raise e
                if attempt == self.retries - 1:
                    raise
                if e.code == 429:
                    time.sleep(6 * (attempt + 1))
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                if attempt == self.retries - 1:
                    raise
            time.sleep(1.5 * (attempt + 1))
        raise RuntimeError("unreachable")

    def call(self, method, params, timeout=40):
        res = self.raw(method, params, timeout)
        if "error" in res:
            raise RpcError(res["error"])
        return res["result"]

    def query(self, request_type, block=None, **kw):
        params = {"request_type": request_type, **kw}
        if block is None:
            params["finality"] = "final"
        else:
            params["block_id"] = block
        res = self.raw("query", params)
        if "error" in res:
            raise RpcError(res["error"])
        return res["result"]

    def view(self, account, method, args=None, block=None):
        res = self.query(
            "call_function",
            block=block,
            account_id=account,
            method_name=method,
            args_base64=base64.b64encode(json.dumps(args or {}).encode()).decode(),
        )
        if "error" in res:
            raise RpcError(res["error"])
        return json.loads(bytes(res["result"]) or b"null")

    def view_account(self, account, block=None):
        """None when the account does not exist at that block."""
        try:
            return self.query("view_account", block=block, account_id=account)
        except RpcError as e:
            if "UNKNOWN_ACCOUNT" in json.dumps(e.data):
                return None
            raise

    def keys(self, account, block=None):
        return self.query("view_access_key_list", block=block, account_id=account)["keys"]

    def block(self, block=None):
        return self.call("block", {"finality": "final"} if block is None else {"block_id": block})


class Outcome:
    """A finished transaction: the hash, its logs, and the value the last receipt returned."""

    def __init__(self, raw):
        self.raw = raw
        self.hash = raw["transaction"]["hash"]
        self.status = raw["status"]
        self.logs = [l for o in raw["receipts_outcome"] for l in o["outcome"]["logs"]]
        self.gas_burnt = raw["transaction_outcome"]["outcome"]["gas_burnt"] + sum(
            o["outcome"]["gas_burnt"] for o in raw["receipts_outcome"]
        )

    @property
    def ok(self):
        return "SuccessValue" in self.status

    @property
    def value(self):
        v = self.status.get("SuccessValue")
        if not v:
            return None
        return json.loads(base64.b64decode(v))

    def failure(self):
        return None if self.ok else json.dumps(self.status)[:400]

    def events(self):
        out = []
        for line in self.logs:
            if line.startswith("EVENT_JSON:"):
                out.append(json.loads(line[len("EVENT_JSON:"):]))
        return out


class Signer:
    """One key on one account. Nonces are read from the chain, so keep one Signer per key."""

    def __init__(self, rpc, account, key):
        self.rpc = rpc
        self.account = account
        self.key = key
        self.nonce = 0

    def send(self, receiver, actions, wait="EXECUTED_OPTIMISTIC"):
        # A key's allowance is charged the whole prepaid gas up front and the unused part comes back
        # a few blocks after the transaction ends, so a fast second call can bounce and then pass.
        for attempt in range(6):
            try:
                return self._send(receiver, actions, wait)
            except RpcError as e:
                if "NotEnoughAllowance" not in str(e.data) or attempt == 5:
                    raise
                time.sleep(2)
        raise RuntimeError("unreachable")

    def _send(self, receiver, actions, wait):
        info = {}
        for _ in range(20):
            # A key added a moment ago reaches optimistic state first.
            res = self.rpc.raw("query", {"request_type": "view_access_key", "finality": "optimistic", "account_id": self.account, "public_key": self.key.public_str})
            info = res.get("result", {})
            if "nonce" in info:
                break
            time.sleep(0.5)
        if "nonce" not in info:
            raise RpcError({"name": "NO_KEY", "account": self.account, "key": self.key.public_str})
        block_hash = b58decode(self.rpc.block()["header"]["hash"], 32)
        # The final state lags the last transaction, so the chain alone can hand out a stale nonce.
        self.nonce = max(info["nonce"], self.nonce) + 1
        tx = (
            bstr(self.account)
            + _pk(self.key.public)
            + u64(self.nonce)
            + bstr(receiver)
            + block_hash
            + u32(len(actions))
            + b"".join(actions)
        )
        digest = hashlib.sha256(tx).digest()
        signed = tx + u8(0) + self.key.sign(digest)
        encoded = base64.b64encode(signed).decode()
        res = self.rpc.raw("send_tx", {"signed_tx_base64": encoded, "wait_until": wait}, timeout=70)
        tx_hash = b58encode(digest)
        if "error" in res:
            text = json.dumps(res["error"])
            if "TIMEOUT" not in text.upper():
                raise RpcError(res["error"])
            return self._poll(tx_hash)
        return Outcome(res["result"])

    def _poll(self, tx_hash):
        for _ in range(40):
            res = self.rpc.raw(
                "tx",
                {"tx_hash": tx_hash, "sender_account_id": self.account, "wait_until": "EXECUTED_OPTIMISTIC"},
                timeout=30,
            )
            if "result" in res and "receipts_outcome" in res["result"]:
                return Outcome(res["result"])
            time.sleep(1.5)
        raise RuntimeError(f"transaction {tx_hash} did not finish")

    def call(self, receiver, method, args=None, tgas=100, deposit=0):
        return self.send(receiver, [act_call(method, args or {}, tgas * TGAS, deposit)])

    def create_subaccount(self, name, balance, key=None):
        """Makes `name` with `key` as its full-access key. `name` is the whole account id."""
        key = key or self.key
        actions = [act_create_account(), act_transfer(balance), act_add_full_key(key.public)]
        return self.send(name, actions)

    def deploy(self, code):
        return self.send(self.account, [act_deploy(code)])
