# Espalier design

An espalier is a fruit tree trained against a frame: every branch grows only where the frame lets
it. Here the frame is a NEAR contract. An owner gives an agent a budget, and the agent can pass part
of that budget to sub-agents, each of which can pass part of its own further down. A branch can only
be narrower than the branch it grows from, and cutting a branch cuts everything beyond it.

## Model

- The **vault account** holds the funds and runs the contract.
- A **grant** is a node in a forest. It has a parent (roots have none), terms, and counters.
- Each grant has exactly one **function-call key**, added to the vault's own account by the contract
  with `AddKey`, and removed with `DeleteKey`. The agent generates the keypair and only the public
  key ever goes on chain, so a parent never holds a child's secret.
- A signed call arrives with `predecessor == signer == vault`. The contract maps
  `env::signer_account_pk()` to the grant. No method takes a grant id as the caller's identity, so a
  child has nothing to forge.

## Terms and attenuation

`delegate` refuses a child that is wider than its parent in any dimension. Each refusal has its own
reason code, and each has a scenario in `harness/src/scenarios.rs`.

| Dimension | Rule for a child | Code | Scenario |
|---|---|---|---|
| asset | same as parent | `ATT_ASSET` | A14 |
| cap | at most the smallest remainder on the lineage | `ATT_CAP` | A01, A16 |
| per_tx | at most parent | `ATT_PER_TX` | A02 |
| window_cap | at most parent | `ATT_WINDOW_CAP` | A03 |
| window_secs | at least parent: the same amount over a shorter window is a higher rate | `ATT_WINDOW_LEN` | A04 |
| approval_threshold | at most parent | `ATT_THRESHOLD` | A05 |
| recipients | subset of parent; only a root can hold "any" | `ATT_RECIPIENTS` | A06, A07 |
| methods | subset of parent; also written into the FC key, so the protocol enforces it | `ATT_METHODS` | A08, B09 |
| expires_at_ms | at most parent | `ATT_EXPIRY` | A09 |
| max_depth | at most parent minus 1 | `ATT_DEPTH` | A10 |
| max_denials | at most parent | `ATT_DENIALS` | A11 |
| key_allowance + gas_budget | carved out of parent's gas budget | `ATT_GAS` | A12 |
| node_budget + 1 | carved out of parent's node budget | `ATT_NODES` | A13 |

Terms never change after creation, because no method writes them. That is what lets the leaf's
recipient list and expiry stand in for all its ancestors' at spend time: the subset relation was
proved once, and it cannot go stale.

## Spending: what makes the subtree bound hold

Caps are not carved out of the parent. Two children can each be promised the parent's whole budget.
Instead every spend is charged to **every grant on its lineage**, and each of those grants must have
room at that moment:

```
for each grant from leaf to root:
    amount <= cap - spent - reserved
    amount <= window_cap - window_used
```

So `spent + reserved <= cap` holds at every grant, and the counter at a grant is the total of its
whole subtree. The bound "a subtree never exceeds any ancestor's remaining allowance" is therefore
not a separate mechanism: it is the same loop. Scenario A17 shows oversubscribed siblings stopping
exactly at the parent's cap, and A18 shows a mid-tree ancestor binding a leaf that has room of its own.

NEAR runs receipts for one account one at a time, so check-then-reserve inside a single receipt is
atomic. The gap that does exist is between the reservation and the callback, which is what the
`reserved` counter covers: a second spend sees the first one's reservation at every level.

```
request_spend (one receipt)
  identify key -> check lineage live -> check policy -> check headroom on every level
  -> check vault balance minus storage        (nothing has been written up to here)
  -> reserve on every level, record Pending
  -> transfer or ft_transfer -> on_spend_settled
on_spend_settled (a later receipt)
  success: reserved -> spent on every level
  failure: release reserved and the window on every level, restore the token ledger
```

The vault balance check is `account_balance - (storage_usage + 4096 bytes) * byte_cost - gas_floor`.
Native funds in flight have already left `account_balance`, so they need no separate tracking. Token
balances are tracked in a ledger that is debited at reservation and credited back on rollback.

If `on_spend_settled` itself ever failed, the reservation would stay: it would consume budget and no
funds would be double-spent. That failure is on the safe side. The callback has 20 Tgas attached
statically; the measured need at maximum depth is in `docs/evidence/gas.json`.

## Rolling window

`contract/src/window.rs`. The window is 16 buckets of width `ceil(W/16)`, and `used()` sums 17 of
them. That always covers `(now - W, now]`, so the sum is never below the true rolling total and no
interval of length W can exceed the cap. A spend can stay counted for one bucket width longer than W,
which can only refuse, never over-allow. There is no period boundary, so the 2x burst of a fixed reset
does not exist (E08). Storage per grant is at most 17 entries.

## Cascading revoke and why its gas does not depend on the subtree

`revoke(g)` writes one flag. There is no loop over descendants, because descendants do not need
telling: every path that can move funds or extend the tree walks the lineage (`check_live`), and it
fails at the revoked ancestor. That walk is already needed for the cap check, so revocation adds no
extra reads. Tree height is capped at 8 levels, so a walk touches at most 9 grants.

Grants are never un-revoked, so a walk can never find a stale "alive".

Deleting the keys is separate work, and it is the only part that scales with subtree size. It has
three bounded paths:

1. `revoke` itself runs a sweep of 32 work units, so a chain of 8 or a small tree loses all its keys
   in the same transaction (C01).
2. The rest stays in a persistent FIFO queue. `sweep(limit)` continues it, up to 64 units a call, and
   anyone can call it because it only touches keys of revoked grants (C03).
3. A dead key that tries anything gets a refusal, and that refusal deletes the key (C03, C08).

A key that has not been reached yet is harmless: it can sign, the contract refuses, and the attempt
reaps it. What it costs is at most one denied call of gas. Each key gets its own `DeleteKey` receipt
because the actions of one receipt are atomic, so one bad key in a shared batch would keep the others
alive.

Measured gas is in `docs/evidence/gas.json` and summarised in the README.

## Key lifecycle and gas drain

Agent keys live on the vault account, so their gas is paid from the vault. There are three layers.

1. **Protocol allowance.** Each key has a protocol-level allowance, and the total the tree can hand
   out is bounded by the root's `gas_budget`, carved at each level. This is a hard stop that no
   contract bug can move (F02).
2. **Denial counter.** Denials return normally instead of panicking, so the counter survives. After
   `max_denials` the vault revokes the grant and deletes the key (F01, F08). The counter does not
   reset on a success, so a key cannot launder failures with dust spends. It never resets at all: a
   grant that reaches its limit needs a new grant.
3. **Refill.** A key with an exhausted allowance is locked out. The protocol cannot raise an allowance
   in place, so `refill_key` deletes and re-adds the same key in one receipt, charged to the
   ancestor's budget (F03). Only an ancestor or the owner can refill, so a compromised key cannot
   extend its own life.

Sizing an allowance: the protocol charges `prepaid gas x gas price` up front against the allowance and
refunds what was not burnt, so a key must always hold a full call's worth. The sandbox prices gas at
1e9 yocto (100 Tgas needs 0.1 NEAR of allowance). A key below that is refused before the contract
runs, whatever it has burnt so far.

**Known limit (F04).** A call with arguments that fail to parse panics in the generated entry point
before any contract code runs, so it burns gas and does not move the counter. The same goes for a
call with too little gas to reach it. Those attempts are bounded only by layer 1.

## Approvals

An amount above `approval_threshold` goes to a queue instead of being spent. The queue entry stores a
sha256 over `(vault, grant, asset, recipient, amount, memo, nonce)`, and the nonce is never reused.

- `approve` takes the hash, so the approver commits to the exact spend it read.
- `execute_approved` takes the arguments again and hashes them, so nothing else can execute (D03).
- Every limit is evaluated again at execution (D05, D06). The entry is removed before funds move, so
  a replay finds nothing (D02).

Approval authority climbs the tree. An ancestor can approve what it could have spent without approval
itself, and anything above every agent's threshold needs the owner (D07). This is not a privilege
increase: that ancestor could have made the same payment from its own key.

The queue holds at most 8 entries per grant, so it cannot be used to fill storage (D10).

## Identity checks that are easy to get wrong

| Trap | Why it bites here | What the contract does |
|---|---|---|
| `#[private]` on callbacks | An agent key on the vault also arrives with `predecessor == current_account` | No issued key ever lists a callback name; `method_names` can only produce names from `METHODS` (B07) |
| Owner equal to the vault | Every agent call would pass the owner check | `new` refuses it (B11) |
| Trusting the signer | A token called during a spend can call back with `signer == vault` and the agent's real key | `agent()` also requires `predecessor == vault` (B06) |
| Trusting the public key | The same key can be registered on another account | `agent()` also requires `signer == vault` (B05) |
| Empty `method_names` | At protocol level it means every method | `check_terms` refuses it (A15) |
| Reading a long promise result | A too-long result is an error from the SDK, yet the call succeeded | Only `PromiseError::Failed` counts as failure |

## Storage

A grant stores about 400 bytes plus its recipient list, and an agent can create grants without
attaching a deposit, because FC keys cannot attach one. Storage is therefore paid by the vault, and
`node_budget` is what bounds it: a root's whole tree can hold at most `node_budget + 1` grants, at
most 32 children per grant, 16 recipients per list and 8 levels.
