# Swarm rules: a sealed-bid group buy where every bid is backed by a delegated budget

**Status: design.** Written before any market code, as the swarms track prompt asks ("Start with RULES.md
and a sequence diagram of one round in text. Wait for my OK before writing contracts."). Nothing in this
file is built yet. The only thing that has run is a throwaway probe that checks the assumptions below
against the committed, unchanged vault wasm on a real local node: 34 of 34 checks passed
([evidence/market-probe.log](evidence/market-probe.log), source in
[evidence/market-probe/](evidence/market-probe/)). How it will be built and tested is in
[SWARM-BUILD.md](SWARM-BUILD.md).

The vault is not changed, not by one line. The market is a second contract. It uses four views the vault
already has and receives money through the vault's existing `request_spend`.

## 1. What it is

Buyer agents each stand for one user and want a few units of the same good (the demo good is a GPU-hour
voucher). Alone each pays the list price. Together they are an order big enough for sellers to compete
for, and big enough to reach a bulk price. The buyers pool their orders in a round, seller agents answer
with sealed offers, and one price clears the round. Each buyer pays only for its own units.

What is new here is where the buyers' money comes from. No buyer agent holds funds. Each one holds a
grant in an Espalier vault: a node in a budget tree that can only be narrower than its parent. A bid
exists only if the vault first agreed to pay for it, which means the grant and every ancestor had room
at that moment. Cutting a branch of the tree cuts its bids.

## 2. Who takes part

| Role | On chain | Can do | Cannot do |
|---|---|---|---|
| Principal | Owner account of a vault | Fund the vault, create root grants, name the sellers its agents may buy from and the highest unit price, revoke anything | |
| Team lead | A grant with `delegate` and `revoke` | Hand part of its budget to buyer agents, revoke them | Give a buyer more than it has, in any of the 14 dimensions |
| Buyer agent | A non-root grant whose key may call `request_spend` only, and whose recipient list holds the market | Deposit into the market, sign orders | Pay anyone else, hold funds, withdraw, send a transaction to the market |
| Relayer | Any account. In the demo each buyer has its own sub-account, `buyer-7.<base>`, holding gas only | Carry a buyer's signed message to the market | Change it: the market checks the grant key's signature |
| Seller agent | Its own sub-account, `seller-3.<base>` | Stock goods, post a bond, make one sealed offer per round, collect its proceeds | Offer in a round that does not list it |
| Opener | Any account, `coordinator.<base>` in the demo | Open a round and pay its storage fee | Anything after that. The market has no admin |
| Anyone | | Call `clear`, `void_order`, `sync`, `claim_goods`, and `refund` for a grant that is no longer live | |

One buyer agent is three things that the market ties together: a grant id in a vault, the key the vault
issued for that grant, and a gas-only sub-account that relays for it. The key is the identity. The same
key that signs `request_spend` on the vault signs the agent's orders, and the market reads the public
half from the vault each time, so a key the vault has rotated or a grant it has revoked stops working in
the market at once.

## 3. Money

- **Deposit.** A buyer agent calls `request_spend(recipient = market, amount)` on the vault with its own
  key. The vault applies every limit it has: the grant's cap, per-transaction limit and rolling window,
  the same three on every ancestor, the recipient list, expiry, the denylist screen. If it passes, native
  NEAR moves from the vault to the market account.
- **Credit.** The market learns about the deposit by asking the vault: `contact(grant, market)` returns
  the total the vault has ever settled from that grant to the market. The market credits the difference
  since it last looked. It never credits on anyone's word, and never twice.
- **Lock.** Placing an order moves part of the credit into a lock for that round. The lock is the most
  the order can ever cost.
- **Paid, returned.** At clearing, a filled order pays `price x quantity` out of its lock, and the rest of
  the lock goes back to credit. Credit can be used in a later round, or sent back. It can only ever be
  sent to the vault account it came from.

A deposit is charged to the grant and to every ancestor when it leaves the vault. Money that later comes
back raises the vault's balance but does not lower those counters: the vault has no method for that, and
this design does not add one. So the tree's counters show what the agent was allowed to commit, which is
at least what it paid. Section 11 says more.

## 4. One round

A round has three windows, set when it is opened: `order_end`, `offer_end`, `reveal_end`.

```text
Principal   Team lead   Buyer agent    Relayer       Vault            Market           Seller      Anyone
   |            |        (grant key)  (buyer-N)   (unchanged)           |                 |           |
   | create_root, set_mandate(sellers, max price) ---------------------->|                 |           |
   |----------->| delegate(buyer key, narrower terms) -->|               |                 |           |
   |            |            |            |              |               |<-- stock goods -|           |
   |            |            |            |              |  open_round(ceiling, sellers, bond, windows)|
   |            |            |            |              |               |<----------------------------|
   |            |            |            |              |               |                 |           |
   |            |            | request_spend(market, amount)             |      ORDER WINDOW           |
   |            |            |-------------------------->| limits on the grant and every ancestor      |
   |            |            |            |              |-- NEAR ------>|                 |           |
   |            |            | sign(round, lock, hash of qty/limit/salt) |                 |           |
   |            |            |----------->| place_order ---------------->|                 |           |
   |            |            |            |              |<- get_grant --| live? key? root?            |
   |            |            |            |              |<- contact ----| how much was really paid?   |
   |            |            |            |              |               | order recorded, lock taken  |
   |            |            |            |              |               |                 |           |
   |            |            |            |              |               |      OFFER WINDOW           |
   |            |            |            |              |               |<- place_offer(hash) + bond -|
   |            |            |            |              |               |   (sees orders and locks,   |
   |            |            |            |              |               |    not quantities or limits)|
   |            |            |            |              |               |                 |           |
   |            |            |            |              |               |      REVEAL WINDOW          |
   |            |            |----------->| reveal_order(qty, limit, salt)->|              |           |
   |            |            |            |              |<- get_grant --| still live?     |           |
   |            |            |            |              |               |<- reveal_offer(ask, capacity,
   |            |            |            |              |               |    min units, salt): goods locked
   |            |            |            |              |               |                 |           |
   |            |            |            |              |               |<------------------ clear ---|
   |            |            |            |              |               | one price; filled orders pay,
   |            |            |            |              |               | the rest of each lock returns
   |            |            |            |              |<-- goods -----|-- proceeds ---->|           |
   |            |            | sign(refund)->| refund -->|<-- NEAR ------|                 |           |
```

At any point the principal or a team lead can `revoke` a grant on the vault. Section 6 says what that
does to an order at each stage.

## 5. Rules

**R1. Listing.** The market is created with a fixed list of vault accounts and one goods token. It
credits deposits only from those vaults. Nobody can change the list later.

**R2. Mandate.** A vault's owner tells the market which seller accounts its agents may buy from and the
highest unit price they may be charged. The market confirms the caller is the owner by asking the vault
(`vault_info`). Without a mandate, no grant of that vault can place an order.

**R3. Opening.** Anyone opens a round with: a ceiling price per unit, the list of sellers that may offer
(at most 8), the seller bond, the three windows, and the no-reveal penalty in basis points of the lock
(at most 2000). The opener pays a fixed storage fee. A round holds at most 32 orders.

**R4. Ordering.** During the order window a buyer places one order per round: a lock amount and a
commitment, `sha256("espalier-market-order-v1", market, round, vault, grant, quantity, limit, salt)`.
The message `("espalier-market-bid-v1", market, round, vault, grant, lock, commitment)` must be signed
by the grant's key. The market then asks the vault and accepts the order only if all of these hold:

- the vault is listed, the grant exists, is not a root, pays in native NEAR, and is live (not revoked
  or expired, and neither is any ancestor);
- the grant does not expire before the round's `reveal_end`. A child never outlives its parent, so no
  ancestor expires mid-round either, and an expiry cannot be used to walk away from an order for free;
- the signature verifies against the key the vault reports for that grant;
- every seller in the round is in the vault's mandate, and the round's ceiling is at most the
  mandate's highest unit price;
- after counting what the vault says it has paid, the grant's free credit covers the lock.

An order cannot be changed or withdrawn.

**R5. Offering.** During the offer window each listed seller may place one sealed offer, a commitment
`sha256("espalier-market-offer-v1", market, round, seller, ask, capacity, min_units, salt)`, with exactly
the round's bond attached. Sellers can see how many orders there are and how much is locked. They cannot
see quantities or limits.

**R6. Revealing.** During the reveal window:

- A buyer's `(quantity, limit, salt)` must hash to its commitment, with `quantity >= 1`,
  `limit <= ceiling` and `quantity x limit <= lock`. The market asks the vault again, and the grant must
  still be live. Anyone may submit a reveal: it carries no authority, only the opening of a commitment.
- A seller's `(ask, capacity, min_units, salt)` must hash to its commitment, with `0 < ask <= ceiling`
  and `min_units <= capacity`, and the seller must hold `capacity` units of the goods token in the
  market. Those units are locked until clearing.

A reveal that fails a check changes nothing, and a correct one can still follow before the window ends.

**R7. Clearing.** After `reveal_end` anyone calls `clear`. It runs once.

1. Sort the valid orders by limit, highest first, earlier order first on a tie. Sort the valid offers by
   ask, lowest first, earlier offer first on a tie.
2. Walk the orders with a running total `Q`, starting at 0. An order joins if the offers that ask at most
   its limit can together supply `Q + its quantity`. If not, it is skipped and the walk continues.
3. If `Q` is 0, there is no trade.
4. Hand the `Q` units to the offers, cheapest first, each up to its capacity. The last offer to get
   units is the marginal offer.
5. If the marginal offer got fewer units than its `min_units`, remove that offer and go back to step 2.
   Only the marginal offer can fall short, because every other filled offer sold its whole capacity.
   Each pass removes one offer, so there are at most 8 passes.
6. The price is the marginal offer's ask. Every order that joined pays `price x quantity`. Every offer
   that got units is paid `price x units`.

The price is never above the limit of any order that joined, never below the ask of any offer that sold,
never above the ceiling, and what buyers pay equals what sellers receive.

Example, prices in mNEAR per unit. Offers: S1 asks 11 for up to 20 units, S2 asks 14 for up to 15, S4
asks 8 for up to 100 but only if it sells at least 60.

| Orders | Walk | Result |
|---|---|---|
| A 10 units at most 20, B 15 at most 15 | Both join, `Q = 25`. Cheapest first gives S4 25 units, under its 60, so S4 is removed. Again: S1 20, S2 5 | Price 14. A pays 140, B pays 210 |
| The same, plus C 20 at most 12 and E 30 at most 10 | All four join, `Q = 75`. S4 takes all 75, over its 60 | Price 8. A pays 80, B 120, C 160, E 240 |
| The first two, plus D 200 at most 18 | A joins. D needs 210 and the offers hold 135: skipped. B joins | Price 14, D gets its lock back |

The second row is the group buy: four buyers reach a price that two could not.

**R8. Settlement.** Clearing moves balances inside the market in one receipt, so it cannot half happen.

- A filled order: `price x quantity` leaves its lock and goes to the sellers, the rest of the lock
  returns to the grant's credit, and the units are recorded as owed to that grant's vault.
- An order that revealed and did not join: its whole lock returns to credit.
- A filled offer: its proceeds and its bond become withdrawable, and unsold locked goods are released.
- An offer that revealed and sold nothing: bond and goods are released.

**R9. Goods.** `claim_goods(vault, grant)` sends the units to the vault account with `ft_transfer_call`.
The vault's existing `ft_on_transfer` keeps them if its owner accepted the token (`add_token`) and
returns them otherwise, in which case they stay claimable in the market. Goods go to the vault and to
no other account.

**R10. Refund.** `refund(vault, grant)` sends the grant's free credit to its vault account. While the
grant is live it needs the grant key's signature, so that nobody else can empty an agent's credit just
before it bids. Once the grant is not live, anyone can call it.

**R11. Penalties.** A penalty exists to pay the side that was let down, and is taken only when that side
has someone in it.

- An order that is not validly revealed by `reveal_end` loses `penalty_bps` of its lock. That amount is
  shared among the offers that revealed, by units sold, or by capacity if nothing sold. With no valid
  offer in the round, nothing is taken.
- An offer that is not validly revealed loses its bond. It is shared among the vaults of the orders that
  revealed, by lock. It is paid to the vault accounts and never becomes agent credit, because credit
  must only ever come from a budget the tree approved. With no valid order, nothing is taken.

## 6. When things go wrong

| What happens | Rule | Where the money ends up |
|---|---|---|
| Deposit is over the grant's cap, a parent's remainder, the window, or the per-transaction limit | The vault refuses it (`CAP`, `ANCESTOR_CAP`, `WINDOW`, `PER_TX`). The market is never involved | Nothing moved |
| Two siblings race for the last of a shared parent's budget | The vault handles receipts one at a time. The first deposit settles, the second is refused | The loser can bid less, with a deposit that fits |
| Grant revoked or expired before it orders | `place_order` is refused: the vault reports it not live | Any credit goes back to the vault, by anyone |
| Grant would expire before the round's `reveal_end` | `place_order` is refused, so no order can lapse mid-round | Credit stays usable in a shorter round, or goes back |
| Grant, or any ancestor, revoked after ordering and before revealing | Its reveal is refused. Anyone calls `void_order`, the market asks the vault, and the order is removed with no penalty | Lock returns to credit, credit to the vault, by anyone |
| Revoked after revealing, before clearing | `void_order` works until `clear` runs. If nobody calls it, the order clears: it was placed, funded and opened while the grant was live | As a normal order, and goods and leftovers go to the vault |
| Revoked after clearing | The trade stands | Goods to the vault, leftover credit to the vault, by anyone |
| Buyer never reveals | R11 penalty on the lock. Revoking the grant and voiding the order avoids the penalty, at the price of the grant | Penalty to the sellers that revealed, the rest to credit |
| Buyer reveals numbers that do not match | Refused. It counts as not revealed unless a correct reveal follows in time | As above |
| Buyer reveals `quantity x limit` above its lock, or a limit above the ceiling | Refused, same as a mismatch | As above |
| Seller never reveals, or reveals without the goods in stock | Bond lost (R11) | Bond to the vaults of the buyers that revealed |
| Seller does not deliver | Cannot happen: a valid offer has its goods locked in the market before clearing, and clearing moves them | |
| No offer at or under any order's limit, or every offer removed by its own minimum | No trade | Locks and bonds released, penalties as in R11 |
| Key rotated in the vault mid-round | The order stands, since it was signed by the key that was valid then. New orders and refunds need the new key | |
| Vault paused mid-round | New deposits are refused by the vault. Orders already funded stand. To pull an agent out, revoke it | |
| Someone replays a signed order | Same round: refused, one order per grant. Another round or another market: the signature does not verify | |
| Someone copies another buyer's commitment | The copy cannot be opened: the hash covers the vault and the grant | The copier pays the no-reveal penalty |
| Vault owner has not accepted the goods token | `claim_goods` returns the units to the market | Goods stay claimable |

## 7. Who enforces what

| Step | Vault contract, unchanged | Market contract | NEAR protocol |
|---|---|---|---|
| A buyer cannot commit more than its branch of the tree allows | Cap, window and per-transaction limit on the grant and on every ancestor, at deposit time | Lock at most credit, credit at most what the vault settled | |
| A buyer key can pay only the market | Recipient list, narrowed at `delegate` and fixed after | | The key's receiver is the vault, and it cannot attach NEAR or add keys |
| Only the grant's agent can place its order | Reports the grant's current key | Verifies an ed25519 signature over market, round, vault, grant, lock and commitment | |
| Revoked or expired agents drop out | `revoke` writes one flag, and the lineage walk reports `live = false` for the whole subtree | Reads `live` at order, at reveal, at `void_order` and at `refund` | The vault deletes the revoked keys |
| Money cannot be sent to an accomplice | | Pays only sellers in the vault owner's mandate, at no more than the mandate's price. Refunds and goods go only to the vault | |
| Bids are sealed | | Commit and reveal, with hashes bound to market, round and bidder | |
| No bid without money, no offer without goods | | Lock before order, goods locked at reveal | |
| Settlement is all or nothing | | One receipt, balances inside the contract | Receipts for one account run one at a time |
| Rules cannot be changed | | No admin method | Contract account locked: no full-access key |

## 8. Every interaction with the vault

All line numbers are `contract/src/lib.rs` at commit `1e59742`. "Probe" means the row ran in the probe
on a real local node. The other rows use vault behaviour that existing scenarios already cover.

| Purpose | Existing method | Who calls | Line | Enough as is? | Checked by |
|---|---|---|---|---|---|
| Fund an order | `request_spend(recipient, amount, memo, clearance)` | The buyer's key, on the vault | 321 | Yes. Plain transfer to the market account: balance rose by exactly the deposit | Probe |
| Check before depositing, for free | `preview_spend(grant_id, recipient, amount, clearance)` | The buyer's client, as a view | 796 | Yes | Probe |
| Learn what a grant really paid | `contact(grant_id, recipient)` | The market, cross-contract | 764 | Yes, for non-root grants. A root's record also holds its descendants' payments, so roots are refused as bidders | Probe |
| Learn if a grant is live, its key, parent, asset | `get_grant(grant_id)` | The market, cross-contract | 697 | Yes. 701 bytes for a grant with one recipient | Probe |
| Confirm who owns a vault | `vault_info()` | The market, cross-contract | 776 | Yes | Probe |
| Return money | A plain transfer to the vault account | The market | | Yes. `available_native` rose by the refund. Grant counters did not move | Probe |
| Deliver goods | `ft_on_transfer`, reached through `ft_transfer_call` on the token | The goods token | 674 | Yes, after the owner's `add_token` (243). The owner takes goods out with `withdraw` (290) | Vault scenarios E02, E10 |
| Build the tree | `create_root` (217), `delegate` (412) | Owner, team lead | | Yes | Probe |
| Pull an agent out | `revoke(grant_id)` | Owner or an ancestor | 434 | Yes. The market saw `live = false` in its next call, for the grant and for a child of a revoked parent | Probe |

Three things the vault cannot do, and what the design does instead:

- **Reserve budget without moving money.** The vault's `reserved` counter lives only between a spend
  and its own callback. So the reservation is real money: the deposit sits in the market as credit, and
  the lock is taken there.
- **Let a contract spend for an agent.** `agent()` (line 854) requires the signer and the predecessor to
  both be the vault account, so no contract can ever call `request_spend`. That is on purpose and stays.
  The agent pushes the deposit. The market only reads.
- **Tell the recipient that a payment arrived.** A native spend is a plain transfer, with no call on the
  recipient. So the market pulls: it asks `contact` and credits the difference.

## 9. Why this needs the delegation tree, and not one wallet per agent

The group-buying projects we read at this hackathon fund a buyer agent in one of two ways: the owner puts
money into that agent's own balance, or the owner sets one flat limit per agent. Either way it is a
wallet per agent. It answers "can this agent overspend its own allowance" and nothing else. With a tree,
five things change. The limit that matters is shared: a team's twenty agents can each be promised the
whole team budget, and the vault still stops their deposits at the team's cap, because every deposit is
charged to every ancestor, so no agent needs a private balance set aside for it and no sum of balances
has to be kept under a limit by hand. Idle money is not spread around: until an agent actually deposits for an
order, the funds sit in the vault, not in twenty hot balances. Cutting off a group is one transaction: a
team lead or the principal revokes one grant and every order below it becomes void in the market, where
separate wallets would need one withdrawal per agent, each racing the round. Authority is delegated by
the agents themselves and can only shrink: a buyer agent can hand a narrower, shorter-lived grant to a
bidding sub-agent for one round without asking the principal, and the market sees that sub-agent's
orders bounded by every level above it. And the money has one way home: refunds, forfeited seller bonds
and goods go to the vault account, never to an agent, because the agent has no account to receive them.

## 10. Gas

Measured by the probe on a local node (neard 2.14.0) at tree depth 1, with the vault's denylist screen on:

| Step | Burnt gas, whole transaction |
|---|---|
| Deposit: `request_spend` to the market, with the registry check | 11.49 Tgas |
| Market asks the vault two views in parallel, verifies an ed25519 signature in the callback and writes two records | 8.45 Tgas |

Estimates, to be replaced by measurements:

| Step | Estimate | Why |
|---|---|---|
| `place_order` | 9 to 13 Tgas | The measured pattern plus the round checks. `get_grant` walks the lineage twice, so it grows with depth. The vault's own spend grows about 0.5 Tgas per level |
| `reveal_order`, `void_order`, `refund` | 7 to 10 Tgas | One view and one callback |
| `place_offer`, `reveal_offer` | 3 to 5 Tgas | No cross-contract call |
| `clear`, 32 orders and 8 offers | 20 to 50 Tgas | Sorting 32 and 8, at most 8 passes of 32 x 8 comparisons, about 45 record writes. Must stay far under the 300 Tgas limit, and will be measured at the largest round |

## 11. Known limits

- **Returned money does not restore budget.** A deposit is charged to the grant and its ancestors. A
  refund puts the NEAR back in the vault but the counters stay, as the probe shows (`spent` 60 mNEAR
  before and after a 25 mNEAR refund). The tree therefore over-counts by whatever came back, and never
  under-counts. An agent keeps this small by depositing only what an order needs and reusing credit in
  the next round.
- **The lock is public.** It is an upper bound on `quantity x limit`. An agent can lock more than it
  needs to blur it, at the cost of the point above.
- **The price is the marginal ask.** A seller that expects to be marginal can ask more than its cost, and
  a full cartel can push the price to the ceiling. The ceiling and the mandate's highest price are the
  buyers' protection then.
- **The last to reveal knows more.** The penalty makes silence cost something. It does not make it
  unprofitable in every case.
- **Revocation reaches the market when the vault is asked.** Between a reveal and `clear`, a revoked
  grant's order stands unless someone calls `void_order`.
- **The market trusts its listed vaults.** A listed vault that lied about `contact` could create credit
  out of nothing and spend other vaults' deposits. The list is fixed at creation, and each listed vault
  should be locked and run the published wasm, which anyone can check from its code hash and key list.
- **The goods are a demo token.** Proving that a real GPU hour was delivered is out of scope.
- **Buyers need a relayer.** A grant key cannot send a transaction to the market, so someone has to
  carry the signed message. Anyone can.
- **Bidder grants are not clearance-bound.** On a clearance-bound grant a deposit with no attestor
  ticket is queued for a human. In the demo the bidder branch turns clearance off, so the age and code
  signals of the recipient check do not apply to the market account. The recipient list and the denylist
  screen still do.
- **Native NEAR only**, one order per grant per round, no cancelling, at most 32 orders and 8 offers.
- **No NEAR Intents.** It has no testnet, and this project spends no real money.

## 12. Options that would need a vault change, not taken

| Option | What it would give | What it would cost |
|---|---|---|
| A credit-back method, so a refund lowers `spent` up the lineage | The tree would be charged exactly each buyer's share | New state in the vault to bound what each recipient may hand back, so the deployed `vault2` could not read it without a migration. All 98 sandbox scenarios, both real-runtime property runs, the 100,000-operation in-process run, the 28 mutants and the 113 testnet transactions would have to be redone, and the property oracle rewritten, because `spent` would no longer only grow |
| A spend that calls the recipient | Deposit and order in one transaction | A new external call path in the spend flow, which is the part of the vault the tests cover most heavily |
| A reserve-and-release method open to other contracts | No money in the market until clearing | A new way for a contract to hold a grant's budget, which is new attack surface in the vault |

The design above needs none of them.

## 13. Where this departs from the track prompt

- **Sub-accounts.** The prompt gives every agent a sub-account so each action traces to one agent. Sellers
  are sub-accounts. A buyer's authority is a vault-issued key and a grant id, which trace further: to the
  agent, its parent, and every budget above it. Each buyer also has a gas-only sub-account that relays
  its messages, so the transaction signer is per agent as well.
- **NEAR Intents.** Not used, for the reason in section 11.
- **Coordinator.** It is the market contract. The account that opens a round has no power over it.
