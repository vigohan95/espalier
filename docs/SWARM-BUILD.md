# Swarm market: what is built and how it is checked

The rules are in [SWARM-RULES.md](SWARM-RULES.md). This file says what exists, what ran, and where the
evidence is. Anything not yet run is marked as planned.

## 1. Feasibility: what the probe showed

The probe is a throwaway contract ([evidence/market-probe/src/lib.rs](evidence/market-probe/src/lib.rs))
and a script ([evidence/market-probe/probe.py](evidence/market-probe/probe.py)), written before the
market. It ran on a local node (neard 2.14.0) against `res/espalier_vault.wasm`, sha256 `3218271c...3a4`,
the same file that runs as `vault2` on testnet. 56 of 56 checks passed:
[evidence/market-probe.log](evidence/market-probe.log).

| Assumption | Result |
|---|---|
| A second contract can ask the vault for a grant and read the answer in a callback | Yes. Two views in parallel, a signature check and two writes: 8.45 Tgas for the whole transaction |
| A message signed by a grant's key verifies on chain against the public key the vault reports | Yes. Another grant's key and another payload are rejected |
| The second contract can tell a vault's owner from anyone else (`vault_info`) | Yes |
| `get_grant` shows everything needed to confirm an award: parent, key, cap, limits, recipients, depth, node budget, lifetime | Yes |
| `grant_of_key` maps a bound key to its grant and a free key to nothing | Yes |
| A chain of grants three levels deep can each name the market as their only recipient | Yes. A child that adds a recipient is refused: `ATT_RECIPIENTS` |
| A root without `request_spend` can hand it to a child | **No**: `ATT_METHODS`. Methods only narrow, so the root keeps `request_spend` |
| A draw at the third level is charged to every level above | Yes: spent 20, 20, 20, 20 after one draw of 20. 12.64 Tgas |
| `direct_spent` is exactly what a grant paid itself, also for a root | Yes. A root's `contact` record adds its whole tree (25 against 5), so the market reads `direct_spent` |
| A key already bound to a grant cannot be bound to a second | Yes: `KEY_IN_USE` |
| An award larger than the awarding grant has left is refused | Yes: `ATT_CAP` |
| Two awards can together exceed the parent, and the vault stops the draws | Yes: the second sibling's draw is refused `ANCESTOR_CAP`, naming the parent |
| A grant cannot draw past its own cap, and cannot pay an outside account | Yes: `CAP`, `RECIPIENT_NOT_ALLOWED` |
| One revoke of a contractor makes all grants below it not live and deletes their keys | Yes. 6.86 Tgas for a subtree of 4. Read `live`, not `revoked` |
| A refund to the vault account raises its balance and does not move any grant's counters | Confirmed. This is the over-count in SWARM-RULES section 2 |
| A clearance-bound grant cannot pay the market without a ticket | Confirmed: queued, `NEEDS_CLEARANCE`. Grants in the chain must not be clearance-bound |

No vault method is missing.

## 2. The contract

`market/`, built to `res/espalier_market.wasm` (338 KB). It does not depend on the vault crate: it parses
the JSON the vault's views return. `market/src/pricing.rs` holds the pricing rule as one pure function,
and `market/fixtures/pricing-vectors.json` holds the vectors that the Rust tests, the Python model and
the live page all run.

| Method | Caller | Asks the vault |
|---|---|---|
| `new(vaults, poster_bond, min_window_ms, draw_grace_ms)` | Deployer, once | |
| `set_roster(vault, workers)` | The vault's owner | `vault_info` |
| `post_task(spec, signature)` with the poster bond | Any carrier | `get_grant` of the poster |
| `commit_bid(task_id, commitment)` with the bid bond | A worker on the roster | |
| `reveal_bid(task_id, price, pk, salt)` | That worker | `grant_of_key` |
| `close_bidding(task_id)` | Anyone | |
| `confirm_award(task_id, grant_id)` | Anyone | `get_grant` of the new grant |
| `default_award(task_id)` | Anyone, after `award_end` | `get_grant` of the poster, `grant_of_key`, then `get_grant` of the key's holder |
| `deliver(task_id, result_hash)` | The winner | |
| `accept(task_id, signature)`, `reject(task_id, reason_hash, signature)` | Any carrier | `get_grant` of the poster |
| `fail_delivery(task_id)` | Anyone, after `deliver_end` | `get_grant` of the winner's grant |
| `void_task(task_id)` | Anyone | `get_grant` of the winner's grant, or the poster's before the award |
| `claim(task_id)` | Anyone | `get_grant` of the winner's grant |
| `refund(task_id)`, `payout(account_id)` | Anyone | |
| `return_unbound(vault, grant_id)` | Anyone | `get_grant` |
| Views: `get_task`, `get_tasks`, `task_of_grant`, `get_roster`, `get_poster`, `balance_of`, `config`, `solvency`, `post_payload`, `accept_payload`, `reject_payload`, `commitment_of`, `price_of` | Anyone | |

Every refusal has a reason code. Calls that ask the vault return it as a value, `{ok: false, code, detail}`.
Calls that do not ask the vault fail with `CODE: detail`, which also returns any attached bond. Every
state change is a NEP-297 event, `standard: "espalier-market"`.

## 3. Scenarios

`harness/src/market.rs`, M01 to M35, in the same harness and report as the vault's scenarios: real signed
transactions against the compiled wasm of both contracts on a real local node. The numbered list with
results is generated into [ATTACKS.md](ATTACKS.md) from the evidence report.

```sh
cargo run --release -p harness -- sandbox --group market --jobs 6
```

Bidding windows are real time, about 30 seconds per auction, so the group takes about 5 minutes.
`MARKET_COMMIT_MS` and `MARKET_REVEAL_MS` lengthen the windows on a slow machine.

## 4. Property test on the real runtime

`harness market-property`: random posts, bids, reveals, awards, deliveries, reviews, draws, claims,
refunds and revokes against both contracts on a local node, with these invariants checked from views
after every round. See section 4 of the evidence report for the run.

| # | Invariant |
|---|---|
| N1 | For every grant in the tree, `spent + reserved <= cap`, and a parent's `spent` is at least the sum of its children's |
| N2 | For every awarded task, `paid <= price`, and `paid + held + refunded` equals the grant's `direct_spent` as last read |
| N3 | The market's balance covers its bonds, balances and held draws, plus its storage |
| N4 | What the market has credited to workers for prices never exceeds what the vault says their grants paid the market |
| N5 | A task whose grant is not live never moves forward, and a final task never changes status |
| N6 | A poster's promised budget equals the sum over its open tasks, and never exceeds what the vault said it could spend when each was posted |
| N7 | Bonds: every bond attached is either still locked, returned, or forfeited to a vault account or to a let-down winner. Nothing else |
| N8 | The contract's winner and price equal the model's on the revealed bids |

## 5. Simulation

`scripts/swarm-sim.py`, standard library only, no LLM calls: one owner, one general contractor, three
levels of hiring, 25 agents with scripted strategies. `--offline` runs the Python model alone,
`--sandbox` sends every action as a signed transaction to a local node, `--network testnet` does the
same on testnet and writes the evidence.

## 6. Testnet

A fresh faucet-funded account, with the vault deployed from the same `res/espalier_vault.wasm`, so its
code hash equals `vault2`'s. One full run of the simulation, including a contractor revoked mid-work
with its subcontractors cut off, and a read-only verifier that looks up every transaction and recomputes
the invariants from views.

## 7. Live page

`site/index.html`: one static file, no backend, no key. The browser reads a public testnet RPC. Checked
on 2026-11-06 with an `Origin` header and a preflight request: `test.rpc.fastnear.com`,
`rpc.testnet.fastnear.com`, `archival-rpc.testnet.fastnear.com` and `near-testnet.drpc.org` all answer
`Access-Control-Allow-Origin: *`.
