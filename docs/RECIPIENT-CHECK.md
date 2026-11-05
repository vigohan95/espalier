# Recipient check

Before an agent pays, something asks about the recipient and answers `allow`, `warn` or `block`, with
reasons. The hard part is not the score. It is that a check running inside the agent's own process stops
protecting anyone the moment the agent is compromised. So the design question for each signal is: can
the contract enforce this, and if not, what does the contract enforce instead?

## Where each signal comes from

| Signal | Source | How it is read | Enforced by |
|---|---|---|---|
| Shared denylist, with evidence | `registry` contract | Cross-contract `check` on every spend (`request_spend` and `execute_approved`) | **Contract**, fails closed |
| Reports below the quorum | `registry` contract | `check` view | Attestor: `warn` |
| History between this agent and the recipient | Vault's own `contact` records, written only at settlement | `contact` view | Attestor: `warn` when too little |
| Account age | Archival RPC: `view_account` at past heights, bisection on existence | `client/espalier/signals.py` | Attestor: `warn`, then contract through the ticket |
| Is it a contract | RPC `view_account`: `code_hash`, and also `global_contract_hash` or `global_contract_account_id`, because an account on a global contract shows an all-zero `code_hash` | Same | Same |
| Code changed recently | Archival RPC: `code_hash` at past heights, doubling steps then bisection | Same | Same |
| Can the code change again | RPC `view_access_key_list`, full-access keys on a contract | Same | Information only |
| Name close to one already paid | Edit distance against names this tree pays | `policy.py` | Attestor: `warn` |
| Amount far above past payments | `contact` records | `policy.py` | Attestor: `warn` |

A NEAR contract can read its own state and nothing about another account: not its age, not its code hash,
not its keys. There is no host function for it. That is why age and code change cannot be checked on chain,
and pretending otherwise would be wrong.

## What the contract enforces instead: tickets

The attestor is a separate process with its own key. When the check says `allow`, it signs a ticket:

```
"espalier-clearance-v1" || borsh(vault, grant, recipient, max_amount, expires_at_ms, code_hash)
```

A grant with `clearance_required` then behaves like this:

| The spend arrives with | Result |
|---|---|
| A valid ticket | Normal path, every limit still applies |
| No ticket | Queued as `NEEDS_CLEARANCE`. No funds move until the owner, or an ancestor not bound by clearance, approves that exact spend |
| A bad ticket (wrong key, wrong recipient, wrong grant, raised amount, expired, over one hour long) | Refused and counted, so repeating it revokes the key |

So a compromised agent that skips its wrapper does not skip the check. It lands in front of a human. A child
grant cannot drop the requirement (`ATT_CLEARANCE`), and a parent bound by it cannot approve around it
(`APPROVER_NEEDS_CLEARANCE`).

What a ticket does not prove: the contract cannot confirm that the attestor looked honestly, or that the
code hash is still current. A ticket lasts 5 minutes by default, and at most an hour. If the attestor key
is stolen, an attacker gets tickets, and then the limits, the allowlist and the denylist are what remain.
No TEE is involved here.

## The shared denylist, and how it resists poisoning

Anyone can file a report: an account, a category, the sha256 of an evidence file, a link to it, and a 0.01
NEAR deposit that comes back on retraction or expiry. The registry decides nothing by itself. Each vault
names the reporters it trusts, up to 16, and a quorum.

| Attack | What stops it | Scenario |
|---|---|---|
| One malicious reporter blocks a competitor | The quorum can never be 1: the vault refuses it, and the registry ignores it | H02, H05 |
| Fresh accounts, as many as you like | Only reporters the vault named are read | H03 |
| Second reporter copies the first one's evidence | Evidence counts once, by hash | H04 |
| The same reporter files twice | One live report per reporter and subject | H04 |
| A flood of reports raises the gas of every check | A check reads at most 16 entries, whatever else was filed | By construction; H03 runs the case with 8 |
| A report with nothing behind it | An evidence hash and a link are required. The contract cannot judge them, so this raises the cost, nothing more | H06 |
| A one-off poisoning lasts forever | Reports expire, at most after 180 days | H07 |
| The listed account clears itself | Its response is public and changes nothing | H08 |
| Reporter's mistake | Retraction ends it, and shows on that reporter's public record | H07 |
| Registry deleted or broken | Fails closed: `REGISTRY_UNAVAILABLE`, not counted against the agent | H09, H10 |

What this does not stop: two colluding trusted reporters can block a recipient. The answer is for the owner
to drop them, and their record is public. A single trusted report only produces a `warn`, so one bad
reporter can cost a human a look, nothing more. The contract does not verify that evidence is true: it holds
a hash, and a person or a tool has to read the file.

## How a spend flows with a registry

```
request_spend (one receipt)
  identify key, check every limit, check the ticket
  reserve on every level, hold the native amount
  -> registry.check(recipient, trusted reporters, quorum)
on_screened (a later receipt)
  not listed and still alive: dispatch the transfer as before -> on_spend_settled
  listed, no answer, unreadable answer, revoked meanwhile, paused meanwhile:
    release every level and the window, emit `denied`
```

Native funds stay in the account while the registry answers, so `native_held` is subtracted from what is
available. Without it, two spends in flight could both promise the same NEAR (H12). Liveness is checked a
second time in the callback, so a revoke that lands during the wait wins (H13, I11 in the property test).

Measured cost on the sandbox: see the screened rows in the README gas table.

## Thresholds

`client/espalier/policy.py`. The defaults are 7 days for account age and code change, and 3 payments over at
least a day for "known". These are judgment calls, not measurements, and the owner can change them. A first
payment always asks a human. Signals are observations: a young account or a fresh upgrade is usually
legitimate, so they end in `warn`. Only a quorum of reporters or a missing account ends in `block`.

## Real mainnet data

`scripts/mainnet-signals.py` reads these signals for real accounts, read-only, and the result is in
[evidence/mainnet-signals.md](evidence/mainnet-signals.md). It uses only public RPC: no key, no transaction.
