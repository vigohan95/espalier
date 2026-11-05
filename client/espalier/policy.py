"""Turns signals into allow, warn or block. Pure functions: same signals, same verdict, no network,
which is what lets `scripts/story.py` re-decide recorded cases offline.

The verdict is about the recipient, not about whether the payment is a good idea. A signal is an
observation, never an accusation: a young account or a fresh deployment is often entirely legitimate,
which is why those end in `warn` and a human, and only a quorum of reporters or a missing account
ends in `block`."""
from dataclasses import dataclass, field

DAY = 86_400


@dataclass
class Policy:
    young_secs: int = 7 * DAY
    code_change_secs: int = 7 * DAY
    #: Payments that must already have settled before a recipient counts as known.
    known_after_payments: int = 3
    #: Span from first to last payment for the same, so that a burst of dust does not count.
    known_after_span_secs: int = DAY
    #: A payment this many times the average so far needs a second look.
    amount_jump_factor: int = 4
    #: Edit distance at which a new name counts as close to a name already paid.
    near_miss_distance: int = 2
    known_names: list = field(default_factory=list)


def edit_distance(a, b):
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def _span(contact):
    return (contact["last_ms"] - contact["first_ms"]) / 1000


def decide(signals, amount, policy):
    """signals: output of `signals.gather` plus optional `history` and `denylist`. Returns the
    verdict and every reason, each with a code a machine can count and a sentence a person can read."""
    reasons = []

    def add(level, code, detail):
        reasons.append({"level": level, "code": code, "detail": detail})

    account = signals.get("account", "?")
    if not signals.get("exists", False):
        add("block", "ACCOUNT_NOT_FOUND", "the account does not exist on this network")
        return _verdict(reasons)

    deny = signals.get("denylist")
    if deny is None:
        add("warn", "SIGNAL_MISSING", "the denylist could not be read")
    else:
        if deny.get("listed"):
            add("block", "DENYLIST_QUORUM", f"{deny['distinct_evidence']} pieces of evidence from trusted reporters")
        elif deny.get("trusted_reports", 0) > 0:
            add("warn", "DENYLIST_REPORTED", f"{deny['trusted_reports']} trusted report, below the quorum")
        if deny.get("all_reports", 0) > deny.get("trusted_reports", 0):
            add("info", "UNTRUSTED_REPORTS", f"{deny['all_reports'] - deny.get('trusted_reports', 0)} reports from reporters this vault does not trust")
        if deny.get("responded"):
            add("info", "SUBJECT_RESPONDED", "the subject has filed a public response")

    age = signals.get("age") or {}
    age_secs = age.get("age_secs")
    if age_secs is None:
        add("warn", "SIGNAL_MISSING", "account age could not be read")
    elif age_secs < policy.young_secs:
        add("warn", "ACCOUNT_YOUNG", f"first seen {age_secs / 3600:.1f} hours ago")

    if signals.get("is_contract"):
        code = signals.get("code")
        if code is None:
            add("warn", "SIGNAL_MISSING", "code history could not be read")
        elif code.get("changed_ago_secs") is not None and code["changed_ago_secs"] < policy.code_change_secs:
            add("warn", "CODE_CHANGED_RECENTLY", f"code hash changed {code['changed_ago_secs'] / 3600:.1f} hours ago at block {code['changed_block']}")
        if signals.get("code_follows_publisher"):
            add("warn", "CODE_FOLLOWS_PUBLISHER", f"runs {signals['code_hash']}, so its code can change with no trace on this account")
        keys = signals.get("keys") or {}
        if keys.get("full_access_keys", 0) > 0:
            add("info", "CONTRACT_UPGRADEABLE", f"{keys['full_access_keys']} full-access keys, so the code can change again")
        elif keys:
            add("info", "CONTRACT_LOCKED", "no full-access key: only the contract itself can change its code")

    hist = signals.get("history") or {}
    contact = hist.get("agent") or hist.get("tree")
    known = bool(contact) and contact["count"] >= policy.known_after_payments and _span(contact) >= policy.known_after_span_secs
    if contact is None:
        add("warn", "NO_PRIOR_PAYMENT", "this tree has never paid this recipient")
    elif not known:
        add("warn", "LITTLE_PRIOR_PAYMENT", f"{contact['count']} payments over {_span(contact):.0f} seconds is not enough to count as known")
    else:
        add("info", "KNOWN_RECIPIENT", f"{contact['count']} payments settled")
        average = int(contact["total"]) // contact["count"]
        if amount > average * policy.amount_jump_factor:
            add("warn", "AMOUNT_JUMP", f"{amount} is over {policy.amount_jump_factor}x the average of {average}")

    if contact is None:
        close = [
            n for n in policy.known_names
            if n != account and edit_distance(n.split(".")[0], account.split(".")[0]) <= policy.near_miss_distance
        ]
        if close:
            add("warn", "NAME_NEAR_MISS", f"close to {close[0]}, which this tree already pays")
    return _verdict(reasons)


def _verdict(reasons):
    levels = {r["level"] for r in reasons}
    verdict = "block" if "block" in levels else "warn" if "warn" in levels else "allow"
    return {"verdict": verdict, "reasons": reasons}
