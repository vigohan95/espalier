#!/usr/bin/env python3
"""One numbered list of attack scenarios, mapped to the threat model, with results taken from the
evidence reports. Fails if a scenario has no threat row, or a threat row names a scenario that did not
pass. Writes docs/ATTACKS.md. Standard library only."""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
E = os.path.join(HERE, "..", "docs", "evidence")

# id, threat, what stops it, enforced by, scenarios
THREATS = [
    ("T01", "Sub-agent asks for more than its parent has, in any dimension", "Attenuation check at `delegate`, one code per dimension", "Contract", "A01 A02 A03 A04 A05 A06 A07 A08 A09 A10 A11 A12 A13 A14 A16 H20"),
    ("T02", "Siblings together overspend a shared parent", "Every spend is charged to every ancestor", "Contract", "A17"),
    ("T03", "Leaf has room but a mid-tree ancestor does not", "The walk checks each level", "Contract", "A18"),
    ("T04", "Empty method list on a key, which means every method", "Refused at `delegate`", "Contract", "A15"),
    ("T05", "Key calls a method outside its grant", "Method list is on the key", "Protocol", "B09"),
    ("T06", "Child revokes its parent or a sibling", "Only strict ancestors can revoke", "Contract", "B01 B03 C04"),
    ("T07", "Child approves its own spend", "Only strict ancestors can approve", "Contract", "B02"),
    ("T08", "A contract relays a call into the vault", "`predecessor` must be the vault", "Contract", "B04"),
    ("T09", "The same key registered on another account", "`signer` must be the vault", "Contract", "B05"),
    ("T10", "Malicious token re-enters during a spend, with the agent's real key as signer", "`predecessor` must be the vault", "Contract", "B06"),
    ("T11", "Agent calls a settlement callback directly", "Callback names are on no issued key", "Protocol", "B07"),
    ("T12", "Key tries to move NEAR, add keys, attach deposit or call another contract", "Function-call key limits", "Protocol", "B08 F07"),
    ("T13", "Stranger uses owner methods", "Owner check", "Contract", "B10"),
    ("T14", "Owner set to the vault, making every agent the owner", "Refused at `new`", "Contract", "B11"),
    ("T15", "Revoked subtree keeps spending or delegating", "Lineage walk, then keys deleted", "Contract and protocol", "C01 C02 C03 C07"),
    ("T16", "Spend in flight during a revoke", "Settles or is refused, and the counters match the money", "Contract", "C06 H13"),
    ("T17", "Revoked grant still has an approved spend", "Lineage re-checked at execution", "Contract", "C05 D09"),
    ("T18", "Expired grant", "Lineage walk; the attempt deletes the key", "Contract", "C08"),
    ("T19", "Approval replayed or used late", "Entry removed before funds move; approvals expire", "Contract", "D02 D08"),
    ("T20", "Spend swapped after approval", "Arguments are re-hashed at execution", "Contract", "D01 D03 D04"),
    ("T21", "Policy changed, budget gone or recipient listed after approval", "Every limit and the registry are evaluated again", "Contract", "D05 D06 H14"),
    ("T22", "Agent approves above its authority", "Approver's own threshold, then the owner; a parent bound by clearance cannot approve around it", "Contract", "D07 H21"),
    ("T23", "Approval queue flooded", "8 per grant", "Contract", "D10"),
    ("T24", "Failed transfer leaks budget", "Callback releases every level and the window", "Contract", "E03 E04 T01"),
    ("T25", "Concurrent receipts race past a limit, or promise the same funds", "Reservation at every level inside one receipt; native funds held during screening", "Contract and runtime", "E06 E07 H11 H12"),
    ("T26", "Payment or withdrawal drains the balance needed for storage", "Check before any state change", "Contract", "E05 E12"),
    ("T27", "Reset boundary allows 2x the window", "Rolling window, no boundary", "Contract", "E08"),
    ("T28", "Payment split to get under a limit", "Rolling window still counts it", "Contract", "E11"),
    ("T29", "Too little gas strands a reservation", "Refused and counted before reserving", "Contract", "E09 H26"),
    ("T30", "Unknown token deposited", "Returned to the sender", "Contract", "E10"),
    ("T31", "Compromised key burns the vault's gas with denied calls", "N denials revoke it, and the allowance is a hard stop", "Contract and protocol", "F01 F02 H23"),
    ("T32", "Denial counter rolled back by a panic", "Denials return normally", "Contract", "F08"),
    ("T33", "Agent locked out by an exhausted allowance", "Ancestor refills, charged to its own budget", "Contract", "F03"),
    ("T34", "Key leaked but grant still wanted", "Rotation in one receipt", "Contract", "F05"),
    ("T35", "Child key collides with a key already on the vault", "`AddKey` failure buries the grant and refunds the parent", "Contract", "F06 F09"),
    ("T36", "Malformed arguments burn gas without moving the denial counter", "Only the allowance bounds it. Known limit", "Protocol", "F04"),
    ("T37", "Injected agent pays a known-bad recipient", "Registry quorum on every spend, at any depth and in any asset; owner blocklist above any allowlist", "Contract", "H01 H25 H27 P02"),
    ("T38", "Shared denylist poisoned", "Consumer-chosen reporters, quorum of at least 2, evidence counted once, expiry, retraction", "Contract", "H02 H03 H04 H05 H06 H07 H08"),
    ("T39", "Registry missing or broken", "Fails closed, not counted against the agent", "Contract", "H09 H10"),
    ("T40", "Agent skips the recipient check", "No ticket means the approval queue", "Contract", "H15 H16"),
    ("T41", "Forged, stale, reused or widened ticket", "Signature binds vault, grant, recipient, amount and expiry; attestor can be replaced", "Contract", "H17 H18 H19 H22"),
    ("T42", "History faked with refused or failed payments", "Only settlement writes a contact", "Contract", "H24"),
    ("T43", "Spending goes unseen or the vault cannot be stopped", "Every decision is an event with numbers; pause stops spending and delegation, not revocation", "Contract", "E01 E02 G01 P01"),
    ("T44", "A level of the supply chain spends more than a level above it", "Every draw is charged to every ancestor; the market refuses a post that promises more than the grant has left", "Vault and market", "M02 M08 M32 M35"),
    ("T45", "A poster creates a grant that is not what the auction decided", "The market reads parent, key, cap, limits and lifetime back from the vault before it binds a grant to a task", "Market", "M01 M16"),
    ("T46", "Someone other than the poster posts, accepts or rejects, or reuses a signed message", "ed25519 signature against the grant key the vault reports; a signed post is used once", "Market", "M05 M21"),
    ("T47", "A revoked, unknown or unfit grant posts a task", "The market reads the poster's grant from the vault first", "Market", "M06 M07 M09"),
    ("T48", "An account the owner did not approve is hired with the owner's money", "Roster set by the vault's owner, who is confirmed by asking the vault", "Market", "M10"),
    ("T49", "A bidder sees other bids, bids out of turn, changes its bid or copies one", "Commit and reveal with windows; the hash names the market, the task and the worker", "Market", "M11 M12 M14"),
    ("T50", "A bidder commits and never reveals", "Its bond goes to the vault account", "Market", "M13 M33"),
    ("T51", "A winner raises its pay through its own bid, or bidders agree on a price", "Second-lowest price; the maximum budget caps a cartel and a lone bidder", "Market", "M03 M04 M15"),
    ("T52", "A poster does not award, awards on worse terms, or the winner's key is squatted", "Poster bond to the winner and a ban after three; a squatted key voids the task with nobody punished", "Market", "M17 M18"),
    ("T53", "A winner is paid without accepted work, or more than its price", "A draw is forwarded only after acceptance, up to the price; the rest goes back to the vault", "Vault and market", "M19 M20 M22 M23"),
    ("T54", "A hijacked contractor pays an outside account", "Its grant can pay only the market, a sub-grant cannot add a recipient, an outsider cannot bid", "Vault and market", "M24"),
    ("T55", "A revoked poster or winner keeps its tasks alive, or a revoke punishes the wrong party", "The market reads `live` from the vault; one revoke cuts the whole branch; a void costs no bond", "Vault and market", "M25 M26 M27 M28 M29 M30"),
    ("T56", "Money gets stuck in the market or the market owes more than it holds", "Stray payments return to the vault account; every bond, balance and held draw is counted", "Market", "M31 M34"),
]


def load(name):
    path = os.path.join(E, name)
    return json.load(open(path, encoding="utf8")) if os.path.exists(path) else None


def main():
    sandbox = load("sandbox-report.json")
    testnet = load("testnet-run.json") or {"scenarios": []}
    by_id = {s["id"]: s for s in sandbox["scenarios"]}
    tn = {s["id"]: s for s in testnet["scenarios"]}
    mapped = {}
    for tid, _, _, _, ids in THREATS:
        for i in ids.split():
            mapped[i] = tid
    missing = [i for i in by_id if i not in mapped]
    unknown = [i for i in mapped if i not in by_id and i not in tn]
    failed = [i for i in mapped if (by_id.get(i) or tn.get(i))["status"] != "pass"]
    if missing or unknown or failed:
        sys.exit(f"no threat row: {missing}; not in any report: {unknown}; not passing: {failed}")
    numbered = []
    for tid, _, _, _, ids in THREATS:
        numbered += [(i, tid) for i in ids.split()]
    lines = [
        "# Attacks and threats",
        "",
        "Generated by `scripts/attacks.py` from the evidence reports. Do not edit by hand. The script stops if a scenario has no threat row, or a listed scenario did not pass.",
        "",
        f"Every scenario is real signed transactions against the compiled wasm on a real node. Most are attacks; a few, such as E01 and G01, show that normal behaviour and its records are right. {len(numbered)} scenarios map to {len(THREATS)} threats, with {sum(1 for i, _ in numbered if i in tn)} also run on NEAR testnet.",
        "",
        "Trust assumptions: the owner account is honest and its key is safe, and the NEAR protocol works as specified. Any agent key can be stolen, and any agent can be prompt-injected. Unless a row says otherwise, the attacker holds a real agent key.",
        "",
        "Threats T01 to T43 are about the vault. T44 to T56 are about the subcontracting market on top of it, scenarios M01 to M35: there the attacker can also be a worker, a poster, or whoever carries a message. Rules: [SWARM-RULES.md](SWARM-RULES.md).",
        "",
        "## Threat model",
        "",
        "| # | Threat | What stops it | Enforced by | Scenarios |",
        "|---|---|---|---|---|",
    ]
    num_of = {i: n + 1 for n, (i, _) in enumerate(numbered)}
    for tid, threat, stop, by, ids in THREATS:
        refs = ", ".join(f"{num_of[i]}" for i in ids.split())
        lines.append(f"| {tid} | {threat} | {stop} | {by} | {refs} |")
    lines += [
        "",
        "## Numbered scenarios",
        "",
        "`Sandbox` and `Testnet` give assertions passed. Transactions for the testnet column: [testnet-run-txs.md](evidence/testnet-run-txs.md).",
        "",
        "| # | Scenario | Threat | What it does | Sandbox | Testnet |",
        "|---|---|---|---|---|---|",
    ]
    for n, (i, tid) in enumerate(numbered, 1):
        s = by_id.get(i)
        t = tn.get(i)
        title = (s or t)["title"]
        a = f"{len(s['facts'])} pass" if s else "-"
        b = f"{len(t['facts'])} pass, {len(t['txs'])} txs" if t else "-"
        lines.append(f"| {n} | {i} | {tid} | {title} | {a} | {b} |")
    lines += [
        "",
        "## Covered outside this list",
        "",
        "- Random operations with invariants: [property-report.json](evidence/property-report.json), [sandbox-property.json](evidence/sandbox-property.json), [sandbox-property-screened.json](evidence/sandbox-property-screened.json).",
        "- A real model under indirect injection: [trials/report.md](evidence/trials/report.md).",
        "- The official attack demo as a log: [attack-demo.log](evidence/attack-demo.log).",
        "- A rogue worker in the hiring flow: [hire-demo-sandbox.log](evidence/hire-demo-sandbox.log).",
        "- Random operations against vault and market together, with invariants: [market-property.json](evidence/market-property.json).",
        "- A 25-agent, three-level subcontracting run with a hijacked contractor, on a local node and on testnet: [swarm-sim-sandbox.log](evidence/swarm-sim-sandbox.log), [swarm-sim-testnet.log](evidence/swarm-sim-testnet.log).",
        "",
    ]
    with open(os.path.join(HERE, "..", "docs", "ATTACKS.md"), "w", encoding="utf8", newline="\n") as f:
        f.write("\n".join(lines))
    print(f"{len(numbered)} scenarios, {len(THREATS)} threats, {sum(1 for i, _ in numbered if i in tn)} on testnet")


main()
