# Swarm market: what will be built and how it will be checked

**Status: plan.** The rules are in [SWARM-RULES.md](SWARM-RULES.md). Nothing below exists yet except the
probe in section 1. Every number in this file that is not from the probe is a target, not a result.

## 1. Feasibility: what the probe showed

The probe is a throwaway contract ([evidence/market-probe/src/lib.rs](evidence/market-probe/src/lib.rs))
and a script ([evidence/market-probe/probe.py](evidence/market-probe/probe.py)). It ran on 2026-11-06 on a
local node (neard 2.14.0) against `res/espalier_vault.wasm`, sha256 `3218271c...3a4`, the same file that
runs as `vault2` on testnet. 34 of 34 checks passed: [evidence/market-probe.log](evidence/market-probe.log).

| Assumption the design rests on | Result |
|---|---|
| A second contract can ask the vault `get_grant` and `contact` in parallel and read both answers in one callback | Yes. 8.45 Tgas for the whole transaction, including the signature check and two writes |
| The key that speaks for a grant can sign a message that the second contract verifies on chain, using the public key the vault reports | Yes. Another grant's key and a different payload are both rejected |
| The second contract can confirm that its caller owns a vault (`vault_info`) | Yes, and it tells a non-owner apart |
| An agent's `request_spend` to a contract account settles as a plain transfer, with the denylist screen on | Yes. 11.49 Tgas. The contract's balance rose by exactly the amount |
| `contact(grant, recipient)` counts exactly what settled from that grant to that recipient | Yes for non-root grants. A root's record also includes its descendants (110 mNEAR for a root that paid nothing itself), so the market must refuse roots |
| Crediting the difference since the last look never credits twice | Yes |
| A deposit over a shared parent's remainder is refused by the vault before the market is involved | Yes: `ANCESTOR_CAP`, naming the parent |
| A bidder key cannot pay anyone but the market, and cannot send a transaction to the market itself | Yes: `RECIPIENT_NOT_ALLOWED`, and the protocol's `ReceiverMismatch` |
| A buyer can ask for free what a deposit would do | Yes: `preview_spend` answers `WITHIN_POLICY` or the refusal code |
| Money sent back to the vault account is usable by the vault | Yes: `available_native` rose by the refund |
| A refund does not move the grant's counters | Confirmed: `spent` and `spendable` unchanged. This is the over-count in SWARM-RULES section 11 |
| A revoke is visible to the second contract in its next call, for the grant and for a child of a revoked parent | Yes. Read `live`, not `revoked`: the vault's eager sweep may or may not have set the child's own flag yet |
| A revoked agent cannot top up | Yes. The vault deleted its key in the same transaction |
| A clearance-bound grant cannot deposit without a ticket | Confirmed: `NEEDS_CLEARANCE`, queued. Bidder branches need clearance off or an attestor ticket for the market |

No vault method is missing for this design. What the vault cannot do, and how the design works around
it, is in SWARM-RULES section 8.

## 2. Contract surface

A new crate, `market/`, built to `res/espalier_market.wasm`. It does not depend on the vault crate. It
parses the JSON the vault's views return. The goods token in tests is the existing
`test-contracts/test-ft`, which already has switches to make transfers fail.

| Method | Caller | Cross-contract calls |
|---|---|---|
| `new(vaults, goods, min_window_ms)` | Deployer, once | None |
| `set_mandate(vault, sellers, max_unit_price)` | The vault's owner | `vault.vault_info` |
| `open_round(item, ceiling, sellers, seller_bond, order_ms, offer_ms, reveal_ms, penalty_bps)` | Anyone, with the storage fee | None |
| `sync(vault, grant_id)` | Anyone | `vault.contact` |
| `place_order(round, vault, grant_id, lock, commitment, signature)` | Any relayer | `vault.get_grant` and `vault.contact` |
| `place_offer(round, commitment)` | A listed seller, with the bond | None |
| `reveal_order(round, vault, grant_id, quantity, limit, salt)` | Anyone | `vault.get_grant` |
| `reveal_offer(round, ask, capacity, min_units, salt)` | The seller | None |
| `void_order(round, vault, grant_id)` | Anyone | `vault.get_grant` |
| `clear(round)` | Anyone | None |
| `refund(vault, grant_id, signature)` | Any relayer. Signature needed while the grant is live | `vault.get_grant`, then a transfer to the vault |
| `claim_goods(vault, grant_id)` | Anyone | `goods.ft_transfer_call` to the vault |
| `withdraw()`, `withdraw_stock(amount)` | A seller | A transfer, or `goods.ft_transfer` |
| `ft_on_transfer` | The goods token | None |
| Views: `round`, `rounds`, `orders`, `offers`, `account`, `seller`, `mandate`, `solvency`, `config`, `order_commitment`, `offer_commitment`, `preview_clear` | Anyone | None |

Every refusal carries a reason code, as the vault's do: `VAULT_NOT_LISTED`, `UNKNOWN_GRANT`,
`ROOT_NOT_ALLOWED`, `GRANT_NOT_LIVE`, `EXPIRES_TOO_SOON`, `WRONG_ASSET`, `BAD_SIGNATURE`, `NO_MANDATE`, `SELLER_NOT_APPROVED`,
`PRICE_OVER_MANDATE`, `LOCK_OVER_CREDIT`, `ALREADY_ORDERED`, `ROUND_FULL`, `WRONG_PHASE`, `BAD_REVEAL`,
`OVER_LOCK`, `OVER_CEILING`, `NO_STOCK`, `NOT_LISTED_SELLER`, `WRONG_BOND`, `NOT_OWNER`,
`ALREADY_CLEARED`, `STILL_LIVE`. Every state change is a NEP-297 event, `standard: "espalier-market"`.

The clearing rule is one pure function. The same test vectors, in one JSON file, are run by the Rust
unit tests, by the Python model used in the simulation, and by the JavaScript in the live page.

## 3. Scenarios

Each is real signed transactions against the compiled wasm of both contracts on a real local node, added
to the existing harness and report. Target: all 46. Floor: 28, which must include every row of the
revocation table.

**Funding and identity**

| # | Scenario | Expected |
|---|---|---|
| M01 | Deposit, then `sync` twice | Credit equals the deposit after the first, unchanged after the second |
| M02 | A fake vault contract that reports a huge `contact` total | `VAULT_NOT_LISTED`, no credit |
| M03 | A root grant tries to order | `ROOT_NOT_ALLOWED` |
| M04 | Lock above credit, and the same credit locked in two open rounds | `LOCK_OVER_CREDIT` both times |
| M05 | Three siblings, each promised the parent's whole cap, all deposit the maximum | The vault stops them at the parent's cap with `ANCESTOR_CAP`, and total market credit equals that cap |
| M06 | Order signed by a sibling's key, and by the parent's key | `BAD_SIGNATURE` |
| M07 | A signed order replayed in the same round, in another round, and on a second market | `ALREADY_ORDERED`, `BAD_SIGNATURE`, `BAD_SIGNATURE` |
| M08 | The relayer raises the lock or swaps the commitment | `BAD_SIGNATURE` |
| M09 | The vault rotates the grant's key mid-round | Old key's new order refused, new key accepted, the earlier order still reveals |
| M10 | The grant key sends a transaction straight to the market | Protocol refuses: `ReceiverMismatch` |
| M11 | A grant that does not exist, and a grant paying in a token | `UNKNOWN_GRANT`, `WRONG_ASSET` |

**Mandate**

| # | Scenario | Expected |
|---|---|---|
| M12 | `set_mandate` by an agent, a stranger and the owner | `NOT_OWNER` twice, then accepted |
| M13 | Order in a round that lists a seller outside the mandate, or with a ceiling above the mandate price | `SELLER_NOT_APPROVED`, `PRICE_OVER_MANDATE` |
| M14 | An injected buyer and an accomplice seller: direct payment, then a round the accomplice opened | Vault: `RECIPIENT_NOT_ALLOWED`. Market: `SELLER_NOT_APPROVED`. The accomplice receives 0 |

**Sealed bidding**

| # | Scenario | Expected |
|---|---|---|
| M15 | Order after `order_end`, reveal before `offer_end`, offer outside its window, `clear` before `reveal_end` | `WRONG_PHASE` each time |
| M16 | Reveal with a wrong salt, quantity or limit, then the right one | `BAD_REVEAL`, then accepted |
| M17 | A buyer copies another's commitment | The copy cannot be opened, and pays the no-reveal penalty |
| M18 | Reveal with `quantity x limit` above the lock, and with a limit above the ceiling | `OVER_LOCK`, `OVER_CEILING` |
| M19 | A buyer never reveals | Penalty to the sellers that revealed, in proportion, and the rest back to credit |
| M20 | A seller never reveals | Bond to the vaults of the buyers that revealed, in proportion, and not to agent credit |
| M21 | A seller reveals without the goods | `NO_STOCK`, bond lost |
| M22 | An unlisted seller offers, and a listed one attaches the wrong bond | `NOT_LISTED_SELLER`, `WRONG_BOND` |
| M23 | The last buyer to reveal sees the others and stays silent | It pays the penalty, and the report states its gain and loss side by side |

**Clearing and settlement**

| # | Scenario | Expected |
|---|---|---|
| M24 | The three rows of the example in SWARM-RULES R7 | Prices 14, 8 and 14, with exactly those payments |
| M25 | Two sellers share a round | Price is the marginal ask, nobody pays above its limit, buyers' total equals sellers' total |
| M26 | An oversized order | Skipped, and smaller later orders still join. It needed real budget to place |
| M27 | No offer under any limit, and a round where each offer's own minimum removes it | No trade, everything released |
| M28 | Equal limits and equal asks | Earlier commitment first |
| M29 | The largest round, 32 orders and 8 offers | Clears in one receipt, gas recorded |
| M30 | `clear` twice, by a stranger | Second call `ALREADY_CLEARED`, nothing moves |

**Revocation during a round**

| # | Scenario | Expected |
|---|---|---|
| M31 | Revoked before ordering | `GRANT_NOT_LIVE` |
| M32 | Revoked between order and reveal | Reveal refused, `void_order` removes it with no penalty, anyone sends the credit back, vault balance restored |
| M33 | A team lead revoked with five orders below it | All five void, the round clears with the rest, and the price differs from the same round without the revoke |
| M34 | Revoked between reveal and `clear`, with and without `void_order` | Voided in one case, cleared in the other, goods and leftovers to the vault in both |
| M35 | Revoked after clearing | Trade stands, goods and credit to the vault, the agent's key is gone |
| M36 | A grant that expires before the round's `reveal_end` tries to order, and one that expires after clearing | `EXPIRES_TOO_SOON`. The other trades normally, and after it expires anyone sends its credit back |
| M37 | Revoke and `place_order` sent in the same block | Either refused, or recorded and then voidable. Counters match the money either way |
| M38 | `void_order` on a live grant | `STILL_LIVE` |

**Refunds, goods, and the edges**

| # | Scenario | Expected |
|---|---|---|
| M39 | A stranger calls `refund` for a live grant, then the agent signs one | `BAD_SIGNATURE`, then the vault's balance rises and the grant's `spent` does not move |
| M40 | `refund` while part of the credit is locked | Only the free part leaves |
| M41 | `claim_goods` when the owner accepted the token, and when it did not | Vault's token ledger rises. Otherwise the units come back and stay claimable |
| M42 | The goods token fails a transfer | Units restored, claimable again |
| M43 | Vault paused mid-round | New deposit refused by the vault, funded orders clear |
| M44 | Seller withdraws proceeds and bond, then stock | Exact amounts, twice gives nothing |
| M45 | `open_round` without the fee, with 9 sellers, with a penalty above 2000 | Refused |
| M46 | Two vaults in one round | Each buyer's credit, refund and goods stay with its own vault |

## 4. Invariants for the property tests

An in-process test drives random rounds against an independent model, and a real-runtime test sends
random signed operations to both contracts on a local node, with revokes racing orders.

| # | Invariant |
|---|---|
| N1 | For every grant: credited total is at most `vault.contact(grant, market).total`, and equals credit + locked + paid + penalties + refunded |
| N2 | For every ancestor in the tree: the sum of credited totals below it is at most its `spent`, which is at most its `cap`. The tree's bound holds inside the market too |
| N3 | The market's balance, less storage, covers all credit, locks, seller proceeds, bonds held and amounts owed to vaults |
| N4 | No order pays more than `quantity x limit`, more than its lock, or a unit price above the ceiling or the mandate |
| N5 | In every cleared round, what buyers paid equals what sellers received, and units bought equal units sold |
| N6 | The market's goods balance covers all stock, locked stock and units owed |
| N7 | NEAR leaves the market only to a listed vault account or to a seller listed in the round it was earned in. Never to a relayer, an opener or an agent |
| N8 | A grant that the vault reports as not live never gets a new order, and a voided order never clears |
| N9 | After `clear`, every order and offer is in exactly one final state, and a second `clear` changes nothing |
| N10 | The contract's clearing result equals the model's on random inputs |
| N11 | The amount the market has seen from a grant never goes down |

Mutants: each breaks one line of the market, and the property test must fail. Planned: skip the signature
check, credit the full `contact` total each time, accept roots, ignore `live`, skip the mandate check,
compare the lock with total credit and not free credit, pay the penalty when no offer revealed, send a
forfeited bond to agent credit, set the price to the first ask, skip the `min_units` pass, allow a second
`clear`, send a refund to the caller.

## 5. Testnet evidence

A fresh faucet-funded account with sub-accounts `vault`, `market`, `goods`, `coordinator`, `owner`,
`buyer-1` to `buyer-20` and `seller-1` to `seller-5`. The vault is deployed from the same
`res/espalier_vault.wasm`, so its code hash equals `vault2`'s.

- A subset of about 15 scenarios, chosen to cover each table above, with every transaction recorded.
- Three or more rounds of the 20-buyer, 5-seller simulation, one of them with a team lead revoked mid-round.
- A read-only verifier, like `scripts/verify-testnet.py`: looks up each transaction on a public RPC,
  compares reason codes, recomputes N1, N3, N5 and N6 from views, and recomputes each cleared round from
  its revealed orders and offers.
- Code hashes of both contracts against `res/`, and the access key lists.

## 6. Simulation mode

`scripts/swarm-sim.py`, standard library only, no LLM calls. Three modes with the same strategies:

- `--offline`: the Python model only, no chain, under a second. This is the path for a reviewer with no install.
- `--sandbox`: every action is a signed transaction on a local node.
- `--network testnet`: the same on testnet, writing the evidence files.

One vault. Four team leads under the owner, each with a cap. Five buyer agents under each lead, each
promised more than a fifth of the team's cap, so teams are oversubscribed and the team cap is what binds.

| Buyers | Strategy |
|---|---|
| Truthful | Limit equals its value per unit |
| Shader | Limit 20% under its value |
| Bulk | Large quantity. Its first deposit hits the team cap, so it previews and deposits what fits |
| Silent | Orders and never reveals |
| Hijacked | Tries to pay an outside account, to lock more than its credit, and to order in a round with an unapproved seller. Its team lead then revokes it mid-round |

| Sellers | Strategy |
|---|---|
| Cost-plus | Ask is cost plus a margin |
| Undercutter | Just under the last clearing price |
| Bulk | Low ask, large capacity, high `min_units`: only sells if the pool is big enough |
| No-show | Commits and never reveals |
| Empty | Reveals without stock |

Seller costs and buyer values are invented inputs, stated as such. What the run shows is that the rules
are applied correctly, not what GPU hours cost.

## 7. Live page

One static HTML file with no backend and no build step, for GitHub Pages. The browser talks to a public
testnet RPC directly and never holds a key.

Checked on 2026-11-06 with an `Origin` header and a preflight request: `test.rpc.fastnear.com`,
`rpc.testnet.fastnear.com`, `archival-rpc.testnet.fastnear.com` and `near-testnet.drpc.org` all answer
`Access-Control-Allow-Origin: *` on both. `rpc.testnet.pagoda.co` is retired and answers 403. The page
tries them in that order and says which one it is using. Rate limits are not published, so the page reads
in small batches and falls back to a snapshot file from the repository, labelled as a snapshot.

| Panel | Shows | Source |
|---|---|---|
| Contracts | Vault and market accounts, code hashes against the published ones, access key counts | `view_account`, `view_access_key_list` |
| Tree | Every grant as a node under its parent: cap, charged, live or revoked, key present. Next to it, from the market: credit, locked, paid to sellers, returned to the vault, goods owed | `vault.get_grant` for ids 1 to `vault_info().grants`, `market.account` |
| Rounds | Phase with a countdown. Orders as sealed (lock only), then revealed. Offers the same. Clearing price, fills | `market.round`, `orders`, `offers` |
| Who paid what | Per buyer: locked, paid, back to credit, penalty. Per seller: units, proceeds, bond. Per team: sum against the team cap | `market.account`, `vault.get_grant` |
| Check it yourself | The browser runs the clearing rule on the revealed numbers and compares with the contract's result | The shared rule and vectors |
| Revoke, before and after | The round where a team lead was revoked: the voided orders, the greyed branch, and the price recomputed with those orders left in | Revealed orders of that round, `void_order` events |
| Evidence | Each number links to its transaction | A list in the repository, checked against the RPC on request |

## 8. Build order

1. Market contract with unit tests and the shared clearing vectors. Python model.
2. Harness: deploy both contracts and the goods token, scenarios M01 to M30.
3. Revocation scenarios M31 to M38, goods and refunds M39 to M46, gas at the largest round.
4. Property tests, in process and on the real runtime. Mutants.
5. Python client for the market, simulation in its three modes.
6. Testnet deploy, scenario subset, simulation rounds, verifier.
7. Live page.
8. README, ATTACKS.md, llms.txt, CI job.

If time runs short, cut in this order: mutants for the market, the real-runtime property test (keeping
the in-process one), the second vault on testnet (keeping it on the local node), and the page's
before-and-after panel. Not to be cut: credit from `contact` only, signatures against the vault's key, the
`live` checks, two-sided commit and reveal with the shared vectors, the revocation scenarios, the
offline and local simulation, a testnet run with its verifier, and the page's tree and round panels.
