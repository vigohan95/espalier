# Swarm market: what is built and how it is checked

The rules are in [SWARM-RULES.md](SWARM-RULES.md). This file says what exists, what ran, and where the
evidence is.

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

`harness market-property` throws random posts, bids, reveals, awards with right and wrong terms,
deliveries, reviews, draws, revokes and every call that anyone may make at both contracts on a local node,
in waves. After each wave the invariants are read from views alone, nothing is taken from what a
transaction returned, and a final sweep runs once every deadline has passed.

The committed run ([evidence/market-property.json](evidence/market-property.json)): seed 7, 6 waves, 31
tasks, 671 calls, 1,353 invariant checks, none broken. That is much smaller than the vault's 10,008
operations, because every auction takes real time. There is no in-process version and no mutation test.

| # | Invariant, as checked in `harness/src/market_property.rs` |
|---|---|
| N1 | For every grant, `spent + reserved <= cap`, and its `spent` covers the sum of its children's |
| N2 | `paid <= price`. For every awarded task, `paid + held + refunded` equals what the vault says the grant paid itself, and the grant's cap is the price. A task with no grant holds and pays nothing |
| N3 | Held draws add up across tasks, and the market's balance covers bonds, balances, held draws and storage |
| N4 | Nothing is paid before acceptance |
| N5 | A final task never changes status. After every deadline, the only tasks still open are accepted ones with a live grant |
| N6 | A poster's promised budget is the sum over its open tasks, and a task's `drawn` equals what the vault says its grant and hires spent |
| N7 | Every yocto that came in is locked, owed, held, or was paid out |
| N8 | The contract's winner and price equal an independent model's on the revealed bids |

## 5. Simulation

`scripts/swarm-sim.py`, standard library only, no LLM calls: one owner, one general contractor, three
levels of hiring, 24 workers with scripted strategies, 13 auctions. The script is written once.
`--offline` runs it on the Python model, `--sandbox` sends every action as a signed transaction to a local
node, and `--network testnet` does the same on testnet and writes the evidence. On a node, the last
check compares the chain with what the model predicted for every task, winner, price, payment and grant.

| Mode | Result | Evidence |
|---|---|---|
| Offline | 89/89 checks, under a second | CI runs it on every push |
| Local node | 90/90 checks, 208 transactions, 743 s | [swarm-sim-sandbox.log](evidence/swarm-sim-sandbox.log) |
| NEAR testnet | 90/90 checks, 208 transactions, 2,548 s | [swarm-sim-testnet.log](evidence/swarm-sim-testnet.log), every hash in [swarm-sim-testnet.json](evidence/swarm-sim-testnet.json) |

Costs and budgets are invented inputs. What a run shows is that the rules hold, not what work costs.

## 6. Testnet

`market.espalier-swarm-1106.testnet` and `vault.espalier-swarm-1106.testnet`, on two faucet-funded accounts.
The vault is deployed from the same `res/espalier_vault.wasm`, so its code hash equals `vault2`'s. In the
run the general contractor revokes `hal` while it and three agents below it are working: one transaction,
and all four are cut off.

`scripts/verify-market.py` needs no key. It looks up every transaction, reads every task and grant back,
recomputes winners and prices, and compares the code on chain with `res/`. Its last run is in
[evidence/verify-market.log](evidence/verify-market.log). Regular RPC nodes forget a transaction after
about two days and public archival RPC allows only a few calls a minute, so it reads transactions in
batches from FastNear's indexer, falls back to archival RPC, and retries every call. To check that it can
fail, a copy of the record was altered in four places: a refusal recorded as a success, a transaction that
does not exist, a wrong payment and a wrong cap. It reported all four and exited 1.

`scripts/testnet-reclaim.py` gives a stopped run's NEAR back to its base account. The first testnet attempt
stopped when an account ran out of balance for prepaid gas. Its accounts were reclaimed and the run was
repeated with larger balances and less gas attached. None of its transactions are in the evidence.

## 7. Live page

`site/index.html`: one static file, no backend, no key. The browser reads a public testnet RPC, and falls
back to `site/snapshot.js`, labelled as a snapshot, when none answers. Checked on 2026-11-06 with an
`Origin` header and a preflight request: `test.rpc.fastnear.com`, `rpc.testnet.fastnear.com`,
`archival-rpc.testnet.fastnear.com` and `near-testnet.drpc.org` all answer `Access-Control-Allow-Origin: *`.
It was rendered with headless Chrome at 1180 and 390 pixels wide, live against testnet and from the
snapshot. It is hosted at https://vigohan95.github.io/espalier/.
