"""Gathers what a contract cannot read for itself.

A NEAR contract can see its own state and nothing about another account: not its age, not its code
hash, not its keys. So these signals are gathered here, off chain, from RPC reads only. Every value
says where it came from, and a signal that could not be read is reported as missing rather than as
fine. `policy.py` decides what the signals mean; this file only observes.

Age and code history come from an archival node. `view_account` at a block height says whether the
account existed then and what its code hash was, so both are found by bisection: about 28 reads for
age, about 8 plus 28 for the last code change. No indexer and no API key are involved.

Limits of bisection, which the result reports as `method`:
  * An account deleted and recreated has no single birth: the search returns one boundary.
  * Code can change more than once. The search walks back from now in growing steps and returns the
    most recent boundary it can see at that spacing, so two changes closer together than a step can
    show as one.
"""
from . import near

ZERO_HASH = "11111111111111111111111111111111"


def code_of(view):
    """What code an account runs, from a `view_account` result. An account using a global contract
    shows an all-zero `code_hash`, so that field alone calls it a plain account. Global contracts by
    hash are immutable. Global contracts by publisher account follow whatever that account publishes,
    so their code can change without leaving any trace on this account."""
    if view is None:
        return None
    if view.get("global_contract_hash"):
        return view["global_contract_hash"]
    if view.get("global_contract_account_id"):
        return "global:" + view["global_contract_account_id"]
    return view["code_hash"]


#: First block a mainnet archival node serves (genesis of the current chain).
MAINNET_FIRST_BLOCK = 9_820_210
#: How far back the code search looks, in blocks, doubling from the first step.
FIRST_STEP_BLOCKS = 3_000


def _block_time(rpc, height):
    """Timestamp of a height. Heights can be skipped, so nearby ones are tried."""
    for h in range(height, height + 30):
        try:
            return rpc.block(h)["header"]["timestamp"] / 1e9
        except near.RpcError:
            continue
    return None


def _read(rpc, account, height):
    """The account at a height, trying the next heights when that block was skipped."""
    for h in range(height, height + 30):
        try:
            return rpc.view_account(account, h), h
        except near.RpcError as e:
            text = str(e.data)
            if "UNKNOWN_BLOCK" in text or "NOT_SYNCED" in text or "UnknownBlock" in text:
                continue
            raise
    raise near.RpcError({"name": "NO_BLOCK", "height": height})


def _bisect(rpc, account, low, high, inside):
    """Largest height in [low, high) where `inside` is false, given inside(high) is true."""
    while high - low > 1:
        mid = (low + high) // 2
        value, _ = _read(rpc, account, mid)
        if inside(value):
            high = mid
        else:
            low = mid
    return low, high


def account_age(rpc, account, tip, first_block=MAINNET_FIRST_BLOCK):
    """Height and time at which the account appears. Needs an archival node."""
    now = _block_time(rpc, tip)
    first, _ = _read(rpc, account, first_block)
    if first is not None:
        created, secs = first_block, None
        ts = _block_time(rpc, first_block)
        return {
            "created_block": created,
            "created_at": ts,
            "age_secs": now - ts if ts and now else secs,
            "method": "existed at the first block this node serves, so this is a lower bound",
        }
    low, high = _bisect(rpc, account, first_block, tip, lambda v: v is not None)
    ts = _block_time(rpc, high)
    return {
        "created_block": high,
        "created_at": ts,
        "age_secs": (now - ts) if ts and now else None,
        "method": "bisection on view_account existence",
    }


def code_history(rpc, account, tip, created_block=None, first_block=MAINNET_FIRST_BLOCK):
    """The most recent block where the code hash changed, if any."""
    current = rpc.view_account(account, tip)
    if current is None:
        return None
    code = code_of(current)
    floor = created_block or first_block
    step = FIRST_STEP_BLOCKS
    newer = tip
    samples = []
    while newer > floor:
        older = max(newer - step, floor)
        value, height = _read(rpc, account, older)
        seen = code_of(value)
        samples.append({"block": height, "code_hash": seen})
        if seen != code:
            low, high = _bisect(rpc, account, older, newer, lambda v: code_of(v) == code)
            before, _ = _read(rpc, account, low)
            ts = _block_time(rpc, high)
            now = _block_time(rpc, tip)
            return {
                "changed_block": high,
                "changed_at": ts,
                "changed_ago_secs": (now - ts) if ts and now else None,
                "previous_code_hash": code_of(before),
                "current_code_hash": code,
                "first_deploy": code_of(before) == ZERO_HASH,
                "account_absent_before": before is None,
                "samples": samples,
                "method": "doubling steps back from now, then bisection on view_account code_hash",
            }
        newer = older
        step *= 2
    return {
        "changed_block": None,
        "changed_at": None,
        "changed_ago_secs": None,
        "previous_code_hash": None,
        "current_code_hash": code,
        "first_deploy": False,
        "account_absent_before": False,
        "samples": samples,
        "method": "no change seen at any sampled height down to the floor",
    }


def key_summary(rpc, account):
    keys = rpc.keys(account)
    full = [k for k in keys if k["access_key"]["permission"] == "FullAccess"]
    # A key's nonce starts at about (block height when added) * 10^6 and rises by one for each
    # transaction it signs, so the low digits count the transactions this account has signed.
    signed = sum(k["access_key"]["nonce"] % 1_000_000 for k in keys)
    return {"keys": len(keys), "full_access_keys": len(full), "transactions_signed_estimate": signed}


def gather(rpc, archival, account, first_block=MAINNET_FIRST_BLOCK):
    """Everything readable about an account from RPC alone. `rpc` may be a normal node and
    `archival` a node with history. Missing pieces land in `missing`."""
    out = {"account": account, "missing": []}
    tip = rpc.block()["header"]["height"]
    out["block_height"] = tip
    now_view = rpc.view_account(account)
    out["exists"] = now_view is not None
    if now_view is None:
        return out
    out["balance_yocto"] = now_view["amount"]
    out["storage_usage"] = now_view["storage_usage"]
    out["code_hash"] = code_of(now_view)
    out["is_contract"] = out["code_hash"] != ZERO_HASH
    out["code_follows_publisher"] = out["code_hash"].startswith("global:")
    try:
        out["keys"] = key_summary(rpc, account)
    except (near.RpcError, OSError) as e:
        out["missing"].append(f"keys: {e}")
    try:
        out["age"] = account_age(archival, account, tip, first_block)
    except (near.RpcError, OSError) as e:
        out["missing"].append(f"age: {e}")
    if out["is_contract"]:
        try:
            created = out.get("age", {}).get("created_block")
            out["code"] = code_history(archival, account, tip, created, first_block)
        except (near.RpcError, OSError) as e:
            out["missing"].append(f"code history: {e}")
    return out


def history(rpc, vault, grant, root, recipient):
    """Settled payments, from the vault's own records. Only settlement writes these."""
    leaf = rpc.view(vault, "contact", {"grant_id": grant, "recipient": recipient})
    tree = rpc.view(vault, "contact", {"grant_id": root, "recipient": recipient}) if root != grant else leaf
    return {"agent": leaf, "tree": tree}


def denylist(rpc, screen, recipient):
    """Reads the registry with the reporters this vault trusts."""
    return rpc.view(
        screen["registry"],
        "check",
        {"subject": recipient, "reporters": screen["reporters"], "quorum": screen["quorum"]},
    )
