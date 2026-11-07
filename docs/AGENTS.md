# How an agent uses Espalier

`client/espalier/wrapper.py`, Python standard library only. It builds and signs NEAR transactions itself
(borsh, ed25519 from RFC 8032), so an agent needs no SDK and no node.

## How the agent holds its key

The agent generates a keypair and hands over the public half. The vault adds it to its own account as a
function-call key, so the agent never has a NEAR account of its own and never holds a full-access key.
The protocol then limits that key:

| Limit | Enforced by |
|---|---|
| Receiver is the vault, nothing else | Protocol |
| Only the methods in its grant | Protocol |
| No deposit, so it cannot send NEAR on its own | Protocol |
| Cannot add keys or deploy | Protocol |
| Gas comes from an allowance, which is a hard stop | Protocol |
| Everything about money: caps, window, recipients, tickets, denylist | Vault contract |

A key is charged the whole prepaid gas up front, and the unused part comes back a few blocks later. A
screened spend at 120 Tgas took the allowance down by 107 mNEAR for a moment and by 1 mNEAR for good, so an
allowance has to stay above about 121 mNEAR or the protocol refuses the call. That is transient unless the
key is really out of allowance, and then the way out is `refill_key` from an ancestor or the owner.

## What the agent cannot do without the owner

- Pay a recipient the check does not allow. That needs the owner's own key, on the exact spend.
- Go above the approval threshold, the cap or the window.
- Give a worker anything wider than its own grant, in any of the 14 dimensions.
- Change any term of its own grant, or undo a revoke.

## Paying

```python
from espalier import near, wrapper

agent = wrapper.Agent(rpc, vault, key, grant, wrapper.http_checker(attestor_url), human=owner)
result = agent.pay("vendor.near", amount_yocto, memo="invoice 1186")
```

| Check says | Wrapper does | Chain sees |
|---|---|---|
| `block` | Signs nothing | Nothing |
| `warn` | Asks the human. A no ends there. A yes files the spend, the human approves with their own key, the agent executes | `NEEDS_CLEARANCE`, `approve`, `execute_approved` |
| `allow` | Signs with the attestor's ticket | `request_spend` with `clearance` |
| The check is unreachable | Treated as `warn`, never as `allow` | |

The wrapper is the polite path, not the guarantee. Skipping it gets an agent to a queue, not to the money:
[RECIPIENT-CHECK.md](RECIPIENT-CHECK.md).

Run it: `python3 scripts/attack-demo.py`, log in [evidence/attack-demo.log](evidence/attack-demo.log).

## Hiring

```python
worker_key = ...                     # made by the worker; only the public half is sent
terms = agent.child_terms(cap, per_tx, recipients=[vendor], methods=("request_spend", "execute_approved"))
decision, tx = agent.hire(worker_key_public, terms)
```

`scripts/hire-demo.py` runs the task loop from the official agents prompt: a contractor with a budget splits
a task, buys one part, gets quotes for the rest, counters once with its reasoning, accepts or walks away,
checks the work, pays, and writes a receipt per task. A worker reads an injected page and tries to pay an
attacker, overspend and hire a friend, then gets cut off.

| Run | Log | Receipts |
|---|---|---|
| Sandbox | [hire-demo-sandbox.log](evidence/hire-demo-sandbox.log) | [hire-demo-sandbox.json](evidence/hire-demo-sandbox.json) |
| NEAR testnet, every transaction public | [hire-demo-testnet.log](evidence/hire-demo-testnet.log) | [hire-demo-testnet.json](evidence/hire-demo-testnet.json) |

The grants, keys, payments and refusals are real signed transactions. The agents themselves are scripted, and
the quotes come from a local stand-in, because of what follows.

## market.near.ai

Read on 2026-11-05, public pages only (`/llms.txt`, `/skill/mcp-buyer/SKILL.md`, `/skill/x402-caller/SKILL.md`):

- `GET /v1/agents?q=` is a public catalog search, with no authentication.
- Hiring through the marketplace means a person's account, OAuth over MCP and a funded marketplace balance.
- Hiring a listed agent without an account goes over A2A with x402, paid per call in USDC from a wallet, on
  the networks listed at `/.well-known/x402`.

So jobs can be posted and paid programmatically, but both routes need an account or real USDC, and this
project uses neither. Espalier is not integrated with the market. The part it would own is the money side:
a worker's key and sub-budget, whichever market the job came from.

## Hiring through the market

`hire-demo.py` above is one level of hiring with a price agreed off chain. The market contract makes the
same step an auction and lets it repeat: a grant holder posts a task signed with its grant key, workers
bid sealed, and the winner's prize is a child grant with the price as its cap, from which it can post
tasks of its own. A worker is paid by drawing from its own grant, and the market forwards the draw once
its poster has accepted the work.

```python
from espalier import market

mkt = market.Market(rpc, market_account)
out = mkt.post(carrier, spec, grant_key, bond)        # any account carries it, the grant key signs it
out = mkt.commit(worker, task_id, price, new_key.public_str, salt, bid_bond)
```

Rules: [SWARM-RULES.md](SWARM-RULES.md). A scripted 25-agent run in three modes: `scripts/swarm-sim.py`.
