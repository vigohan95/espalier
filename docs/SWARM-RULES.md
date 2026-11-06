# Swarm rules: a subcontracting market where the prize is a grant

Agents hire agents, who hire agents. Each hire is one sealed-bid round. What the winner gets is not a
payment held in escrow: it is a grant in an Espalier vault, a child of the hirer's grant, narrower in
every dimension, with the auction's price as its cap. The winner can hire from that grant in turn. So
the budget tree is not drawn up by anyone in advance. It grows one node per auction, and every level of
the supply chain is bounded by every level above it, because that is what the vault already enforces.
Cut one contractor and everything it hired is cut in the same transaction.

The vault is not changed, not by one line: `res/espalier_vault.wasm` is the same file as before. The
market is a second contract, `market/`. It calls three of the vault's views and nothing else.

What is built and how it was checked is in [SWARM-BUILD.md](SWARM-BUILD.md). The assumptions were first
tested by a throwaway probe against the unchanged vault wasm, 56 of 56 checks:
[evidence/market-probe.log](evidence/market-probe.log).

## 1. Who takes part

| Role | On chain | Can do | Cannot do |
|---|---|---|---|
| Principal | Owner account of a vault | Fund the vault, create the general contractor's root grant, name the worker accounts its money may hire (the roster), revoke anything | |
| Poster | Any live grant whose only recipient is the market: the general contractor's root, or a grant won in an earlier task | Post a task, create the winner's grant with the vault's `delegate`, accept or reject the work, revoke the winner | Promise more than its grant can still spend. Pay anyone directly |
| Worker | Its own sub-account, `worker-7.<base>`, on the roster. If it wins, it also holds the grant it won | Bid, deliver, draw its price from its own grant after acceptance, post tasks of its own | Draw before acceptance and be paid. Draw more than the price. Pay anyone but the market |
| Carrier | Any account. Each agent's own sub-account carries its signed messages and attaches its bond | Carry a poster's signed post, acceptance or rejection to the market | Change them: the market checks the grant key's signature |
| Anyone | | `close_bidding`, `confirm_award`, `default_award`, `fail_delivery`, `void_task`, `claim`, `refund`, `payout` | Steer any of them: each one asks the vault, or follows a deadline |

A grant key is the identity of a poster. The same key that calls `delegate` on the vault signs the
poster's messages to the market, and the market reads the public half from the vault every time. The
market has no admin and no method that changes its rules.

## 2. Money

There is no escrow of prices. The only money the market ever holds is:

- **Bonds.** A worker attaches a bid bond to each sealed bid. Whoever carries a post attaches a poster
  bond. Bonds go back unless a deadline is missed (section 5).
- **Draws on their way through.** Every grant in the chain has exactly one recipient, the market. A
  winner is paid by calling the vault's `request_spend(market, amount)` with its own grant key. The
  vault charges that draw to the winner's grant and to every ancestor, and refuses it if any of them
  lacks room. The market then reads the grant's `direct_spent` from the vault, counts what is new, and
  forwards it to the winner's account, up to the price, once the poster has accepted the work.

So a task's price is charged to the tree when the winner draws it, in full and once: the vault's
counters equal what left the vault. A contractor's own margin is what is left of its cap after its
subcontractors drew theirs.

A draw the market cannot pay out, because the work was rejected, never accepted, or the amount is over
the price, is held and then sent back to the vault account. That path over-counts: the vault has no
method that lowers a grant's counters when money returns, and none is added. It only happens when a
winner misbehaves, and it burns no more than that winner's own cap.

## 3. One task

```text
Principal   Poster           Carrier        Vault           Market            Workers          Anyone
   |       (grant key)     (agent account) (unchanged)         |           (own accounts)         |
   | set_roster(workers) --------------------------------------->|  asks vault_info: is this the owner?
   |           |                |              |                 |                 |              |
   |           | sign(spec) --->| post_task + poster bond ------>|                 |              |
   |           |                |              |<-- get_grant ---| live? key? only recipient the market?
   |           |                |              |                 | room for max_budget on top of what
   |           |                |              |                 | this grant already promised?
   |           |                |              |                 |                 |              |
   |           |                |              |                 |<- commit_bid(hash) + bid bond  |  COMMIT
   |           |                |              |                 |                 |              |
   |           |                |              |                 |<- reveal_bid(price, key, salt) |  REVEAL
   |           |                |              |<- grant_of_key -| is that key still free?        |
   |           |                |              |                 |                 |              |
   |           |                |              |                 |<----------------- close_bidding|
   |           |                |              |                 | lowest bid wins, pays second-lowest
   |           |                |              |                 |                 |              |
   |           | delegate(winner's key, cap = price) ->|         |                 |              |  AWARD
   |           |                |              | AddKey          |<----------------- confirm_award|
   |           |                |              |<-- get_grant ---| parent? key? cap = price? limits?
   |           |                |              |                 | task Awarded, poster bond returned
   |           |                |              |                 |                 |              |
   |           |                |              |                 |<- deliver(result hash) --------|  WORK
   |           | sign(accept) ->| accept ----------------------->|                 |              |  REVIEW
   |           |                |              |<-- get_grant ---| poster still live? its key?    |
   |           |                |              |                 |                 |              |
   |           |                |   winner: request_spend(market, price) with its own grant key   |  DRAW
   |           |                |              |-- NEAR -------->|                 |              |
   |           |                |              |<-- get_grant ---|<------------------------ claim |
   |           |                |              |                 | counts direct_spent, pays the winner
   |           |                |              |                 |-- payout ------>|              |
```

Between AWARD and its own delivery, the winner can post tasks from the grant it just won. Each of those
runs the same diagram one level down, with the winner as poster.

## 4. Rules

**R1. Listing.** The market is created with a fixed list of vault accounts. It reads only those.

**R2. Roster.** A vault's owner names the worker accounts that may bid on tasks funded from that vault.
The market confirms the caller is the owner by asking the vault (`vault_info`). Without a roster no
task can be posted from that vault.

**R3. Posting.** A post is a signed spec: vault, poster grant, hash of the brief, maximum budget, bid
bond, minimum number of bids, five deadlines (`commit_end`, `reveal_end`, `award_end`, `deliver_end`,
`review_end`), what the winner's grant must carry for subcontracting (levels, node budget, gas budget,
key allowance), and a nonce. The message is `"espalier-market-post-v1"` followed by the borsh encoding
of `(market, spec)`, signed by the poster's grant key. The carrier attaches the poster bond. The market
asks the vault for the poster's grant and accepts the post only if:

- the grant is live, pays in native NEAR, has the market as its only recipient, is not bound to
  clearance tickets, and its key may call `delegate`;
- the signature verifies against the key the vault reports, and this signed post was not used before;
- the grant has the depth, node budget, gas budget and denial limit to create the winner's grant, and
  does not expire before the winner could draw;
- its per-transaction, window and approval limits are at least the maximum budget;
- what the vault says the grant can still spend covers the maximum budget on top of everything this
  grant has already promised to open tasks;
- the grant has fewer than 8 open tasks and fewer than 3 defaults (R7).

**R4. Bidding.** Until `commit_end`, a worker on the roster commits
`sha256("espalier-market-bid-v1", market, task, worker, price, key, salt)` with the bid bond attached.
`key` is the public key the worker wants on its grant if it wins. One bid per worker, at most 16 per
task. Between `commit_end` and `reveal_end` it reveals price, key and salt. The price must be above
zero and at most the maximum budget. The key must not be bound to any other bid, and the market asks the
vault (`grant_of_key`) that it does not already belong to a grant, since such a key could never be
awarded.

**R5. Winner and price.** After `reveal_end` anyone calls `close_bidding`, once. With fewer valid bids
than the task's minimum there is no award. Otherwise the lowest bid wins, the earlier commitment on a
tie, and the price is the second-lowest bid, or the maximum budget when there is only one bid. The
winner's pay does not depend on its own bid, so bidding its real cost is its best move.

**R6. The award.** The poster creates a grant for the winner's key with the vault's `delegate`. Anyone
then calls `confirm_award(task, grant)`, and the market reads that grant from the vault. It is the award
only if all of these hold:

- its parent is the poster's grant, and its key is the key in the winning bid;
- its cap, per-transaction limit, window cap and approval threshold all equal the price;
- its only recipient is the market, it pays in native NEAR, it is live, and it needs no clearance ticket;
- its key may call `request_spend`, and `delegate` and `revoke` if the task promised subcontracting;
- it carries the depth, node budget, gas budget and key allowance the task promised;
- it does not expire before `review_end` plus the draw grace.

A grant that fails any check is not the award, however it was created. The poster can revoke it and try
again until `award_end`.

**R7. No award in time.** After `award_end` anyone calls `default_award`, and the market asks the vault
why there is no award:

- The poster's grant is no longer live: nobody is at fault, the task is void, bonds go back.
- The winner's key now belongs to a grant that someone other than the poster created: the task is
  void, bonds go back.
- Otherwise the poster defaulted: it did nothing, or it created a grant with the wrong terms. Its bond
  goes to the winner, and the default is recorded. A grant with 3 defaults cannot post again.

**R8. Delivery and review.** The winner calls `deliver` with a hash of the result before `deliver_end`.
The poster then signs `accept` or, before `review_end`, `reject`. Silence until `review_end` counts as
acceptance. A rejected or never-delivered task pays nothing.

**R9. Payment.** `claim` reads the winner's grant from the vault, counts what it has drawn into the
market since the last look, and, if the task is accepted, credits the winner up to the price. `payout`
sends an account what it is owed. Anything a task holds that it can never pay goes back to the vault
account with `refund`, once the task is over.

**R10. Revocation.** `void_task` asks the vault whether the task's grant is live: the winner's grant
once there is one, the poster's before. If it is not, the task is void and every bond still locked goes
back. A task with live grants cannot be voided. Being revoked is not a missed deadline, so it costs no
bond.

## 5. Bonds and deadlines

| Missed | Who pays | To whom |
|---|---|---|
| A bid is committed and not validly revealed | That bidder's bond | The vault account the task was funded from |
| The poster does not create the winner's grant by `award_end` | The poster bond | The winner |
| The winner still holds its grant and delivers nothing by `deliver_end` | The winner's bond | The vault account |
| Anything ended by a revoke or expiry | Nobody | Bonds go back |

A forfeited bond is paid to the vault account and never becomes anything an agent can spend.

## 6. When things go wrong

| What happens | Rule | Where the money ends up |
|---|---|---|
| A poster promises more than its grant has left | `post_task` refused: `OVERCOMMITTED` | Poster bond returned at once |
| Two subcontractors of one contractor draw more than the contractor has | The vault refuses the later draw: `ANCESTOR_CAP`, naming the contractor's grant | The later one is paid what was left |
| A contractor draws before it is accepted | The vault allows it inside the cap. The market holds it and pays nobody | Held, then back to the vault if the work is not accepted |
| Poster revoked during bidding | `void_task`: void | Every bid bond and the poster bond go back |
| Winner revoked during the work | `void_task` or `fail_delivery`: void | Winner's bond goes back. Nothing is paid. What it drew goes to the vault |
| A contractor with open subtasks is revoked | One vault transaction makes every grant below it not live. Each of its tasks is voided with one call | All bonds back. Subcontractors already paid keep their pay |
| Revoked after acceptance, before the draw | The winner's key is gone, so it cannot draw | Nothing is paid. Its bond was already returned |
| Revoked after it was paid | The trade stands | The vault's counters equal what left the vault |
| A worker never reveals | Bond lost | To the vault account |
| A reveal does not match, or copies another worker's commitment | Refused: the hash names the worker | A copy can never be opened, and loses its bond |
| A bid names a key that is already in use | Refused: `KEY_TAKEN` | That bid cannot win |
| Someone binds the winner's key to another grant before the award | `default_award` finds it: void, nobody punished | Bonds back |
| The poster creates a grant with a lower cap, tighter limits or a short life | `confirm_award` refuses it. After `award_end` it is a default | Poster bond to the winner |
| The winner delivers nothing | `fail_delivery` | Winner's bond to the vault account |
| The poster rejects good work | Allowed: the contract cannot judge work. The rejection is counted on the poster's record | Winner's bond returned, nothing paid |
| A hijacked contractor tries to pay an outside account | The vault refuses: its grant can pay only the market. A sub-grant cannot add a recipient. An account off the roster cannot bid | Nothing moves |
| A signed post, acceptance or rejection is replayed or altered | Refused: `DUPLICATE_TASK` or `BAD_SIGNATURE` | |
| A root pays the market outside any task | `return_unbound` sends it back | To the vault account |

## 7. Who enforces what

| Property | Vault contract, unchanged | Market contract | NEAR protocol |
|---|---|---|---|
| No level of the chain spends more than any level above it | Every draw is charged to every ancestor | Refuses a post that would promise more than the grant has left | |
| A grant can pay only the market | Recipient list, which a child can only narrow | Posts and awards only for grants whose one recipient is the market | A grant key can only call the vault |
| The winner's grant is exactly the award | `delegate` refuses anything wider than the parent | Reads parent, key, cap, limits and lifetime back from the vault | |
| Only the poster can post, accept or reject | Reports the grant's current key | Verifies an ed25519 signature against it | |
| Payment only for accepted work, and never above the price | Cap equal to the price | Forwards a draw only after acceptance, up to the price | |
| A revoked branch stops | One flag, read on every walk up the tree. Keys deleted | Reads `live` before every step that depends on authority | Deleted keys cannot sign |
| Bids are sealed and binding | | Commit and reveal, hash bound to market, task and worker. Bond | |
| The winner's pay does not depend on its own bid | | Second-lowest price | |
| Rules cannot be changed | | No admin, no upgrade method | |

## 8. Every interaction with the vault

Line numbers are `contract/src/lib.rs` at commit `1e59742`.

| Purpose | Existing method | Who calls | Line |
|---|---|---|---|
| Read a grant: live, key, parent, cap, limits, recipients, depth, budgets, lifetime, what it has paid | `get_grant(grant_id)` | The market, on post, award, review, default, void, claim | 697 |
| Is a key already bound to a grant | `grant_of_key(pk)` | The market, on reveal and default | 745 |
| Who owns a vault | `vault_info()` | The market, on `set_roster` | 776 |
| Create the winner's grant and its key | `delegate(child_pk, terms)` | The poster's grant key | 412 |
| Draw the price | `request_spend(recipient = market, amount)` | The winner's grant key | 321 |
| Cut a contractor and everything below it | `revoke(grant_id)` | The owner or any ancestor | 434 |
| The root grant | `create_root(pk, terms)` | The owner | 217 |
| Cross-check what a grant paid the market | `contact(grant_id, recipient)` | Tests and the verifier, as a view | 764 |

What the vault cannot do, and what the design does about it:

- **A recipient list is fixed when a grant is created, and a child's list must be inside its parent's.**
  So a poster cannot pay a winner it did not know in advance. Every grant in the chain therefore names
  one recipient, the market, and the market forwards a draw to the task's winner.
- **Methods only narrow.** A root without `request_spend` cannot hand it to a child (the probe shows
  `ATT_METHODS`). So the general contractor's root keeps `request_spend`, and a root that pays the market
  by mistake is repaid by `return_unbound`.
- **A root's `contact` record includes its whole tree.** The market does not use it. It reads
  `direct_spent`, which is what a grant paid itself, and which equals what reached the market because the
  market is that grant's only recipient.
- **Caps are not carved out of the parent.** Two awards can each be up to the poster's remainder. The
  vault bounds the total when the money is drawn. The market keeps a running sum of what each poster has
  promised and refuses a post that would go over.
- **No contract can call the vault for an agent.** That stays. The poster delegates and the winner
  draws, each with its own key. The market only reads.

## 9. Why this needs the delegation tree, and not one wallet per agent

A market where each agent has its own funded balance can run one level of hiring: the hirer escrows the
price and the worker is paid from it. The second level is where it stops working. The worker would have
to be paid first to have something to escrow for its own subcontractor, so money moves down the chain
before any work exists, and from then on nobody above can see it, cap it or take it back. With a
delegation tree, nothing moves until work is accepted. A contractor that wins 120 holds a grant of 120,
not 120 in a wallet. It can promise 50 of it to a subcontractor, who promises 20 further down, and each
promise is a child grant that the vault will not let exceed its parent. When the bottom worker draws 20,
that 20 is charged to all three levels above it in the same receipt, so the contractor's margin is
simply what its own cap has left. If the contractor is hijacked, its grant can pay nobody but the
market, and the market pays only the winner of a task that its poster accepted. And when the general
contractor revokes that contractor, one transaction ends the authority of every agent it hired, at any
depth, where separate wallets would mean chasing each balance.

## 10. Known limits

- **A poster can refuse good work.** It can reject, or revoke the winner before the draw. The contract
  cannot judge work. It records rejections and defaults per poster, and a worker can read that record
  before it bids.
- **A contractor can starve its own subcontractor.** If it draws its cap down before an accepted
  subcontractor has drawn, the vault refuses the subcontractor's draw at the contractor's grant
  (scenario M32). The tree's bound holds. The subcontractor is short. Its protection is to draw as soon
  as it is accepted.
- **Subcontractors carry their contractor's risk.** If a contractor is revoked, work in progress below
  it is cut off unpaid, with bonds returned.
- **Promises are bounded by the market, not reserved in the vault.** The market refuses a post that
  over-promises, but a poster could still create grants outside the market. The vault then bounds the
  total at draw time, and someone is paid less.
- **Returned money does not restore budget.** Only on the paths in section 2 where a draw is held and
  sent back.
- **Colluding roster members can still collude.** A cartel can lift a price to the maximum budget. A
  hijacked poster and an accomplice on the roster can run a sham task up to the poster's own cap. The
  budget, the roster and revocation are the limits.
- **A lone bidder is paid the maximum budget.** `min_bids` is the poster's guard.
- **A worker's bid bond and a carrier's poster bond are real NEAR from their own accounts.** Agents
  therefore do hold a little money of their own. It is not the principal's.
- **Delivery is a hash.** Whether the work is good is the poster's judgment.
- **Each step in the market after a revoke needs a call.** The vault cuts a branch in one transaction.
  The market's tasks under it are voided one call each, by anyone.
- **Native NEAR only. No NEAR Intents:** it has no testnet, and this project spends no real money.

## 11. Where this departs from the track prompt

The prompt's example is a group-buying swarm with a sealed-bid round, sub-accounts, a coordinator
contract, a scripted simulation and a live page. This project keeps all of those elements and changes
what is traded: tasks, not goods, and recursively.

- **Sub-accounts.** Every worker is a sub-account. A poster's authority is a vault-issued key and a
  grant, which trace further than an account name: to its hirer, and to every budget above it.
- **NEAR Intents.** Not used, see section 10.

## 12. The design we dropped

The first design in this file was a sealed-bid group buy, with buyer agents funded through the vault.
It was dropped for three reasons. Several teams at this hackathon had already built group buying as
the prompt describes it. The delegation tree only backed the buyers' deposits, so it was a side part.
And every refund of an unused deposit over-counted the tree. In the subcontracting market the tree is
the thing the market produces, and the normal path charges it exactly. The probe results that carried
over are in [SWARM-BUILD.md](SWARM-BUILD.md).
