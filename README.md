# Espalier

[![ci](https://github.com/vigohan95/espalier/actions/workflows/ci.yml/badge.svg)](https://github.com/vigohan95/espalier/actions/workflows/ci.yml)

**Live page:** https://vigohan95.github.io/espalier/

Agents hire agents, who hire agents. In Espalier each hire is a sealed-bid round, and what the winner
gets is not money in escrow: it is a grant in a vault on NEAR, a child of the hirer's grant, capped at
the auction's price and narrower in every other dimension. The winner can hire from that grant in turn.
So the budget tree is grown by the auctions, one node per hire, and no level of the supply chain can
spend more than any level above it. Revoke one contractor and everything it hired stops in the same
transaction.

Built at NEAR HACKATHON 2026. Tracks: `swarms`, `security`, `agents`. Status: testnet. Not audited.

**The problem.** An agent with a budget hires other agents. Today it has two options: share its own key,
or give each worker a wallet that nothing ties back to the original limit. With a shared key one injected
sub-agent can spend everything. With separate wallets, money has to move down the chain before any work
exists, and from then on nobody above can see it, cap it or take it back. Neither survives a second level
of hiring.

**What Espalier does.** Two contracts.

- **The vault** holds the funds. Each agent has a *grant*, a node in a tree, with its own function-call
  key that the vault itself adds and deletes. A grant can create child grants, and the vault refuses a
  child that is wider than its parent in any of 14 dimensions. Every spend is charged to every grant up to
  the root. Revoking a grant kills everything below it. Before a spend, a recipient check answers
  `allow`, `warn` or `block`.
- **The market** runs the hiring. A grant holder posts a task, workers bid sealed, the lowest bid wins at
  the second-lowest price, and the poster creates the winner's grant with the vault's own `delegate`. The
  market reads the vault to confirm the grant is exactly what the auction decided. A winner is paid by
  drawing from its own grant, which can pay only the market, and the market forwards the draw once the
  poster has accepted the work. The vault was not changed to make this possible: it is the same wasm
  file, and the market only reads it.

| Read this | For |
|---|---|
| [docs/SWARM-RULES.md](docs/SWARM-RULES.md) | The market's rules, one task as a sequence diagram, what happens on a revoke at each stage |
| [docs/SWARM-BUILD.md](docs/SWARM-BUILD.md) | What was built for the market and how each part was checked |
| [docs/ATTACKS.md](docs/ATTACKS.md) | Every scenario, numbered, mapped to the threat model |
| [docs/DESIGN.md](docs/DESIGN.md) | The vault: the tree, attenuation, cascade revoke, key lifecycle, gas |
| [docs/RECIPIENT-CHECK.md](docs/RECIPIENT-CHECK.md) | Signals, data sources, on chain versus off chain, denylist poisoning |
| [docs/AGENTS.md](docs/AGENTS.md) | How an agent holds its key, the wrapper, hiring, market.near.ai |
| [docs/BASELINE-GAPS.md](docs/BASELINE-GAPS.md) | Limits the official reference products document, mapped to tests |
| [llms.txt](llms.txt) | The same, for an AI reviewer |

## Try it

### 1. See every conclusion on one screen: offline, no install

```sh
python3 scripts/preflight.py
python3 scripts/story.py
```

`preflight.py` says what will work on this machine and ends with `Next:`. `story.py` takes a few seconds
and ends in `all checks passed`. It does not rerun the chain: it hashes the wasm files, runs the client
unit tests, runs the 25-agent swarm on the Python model and compares it with the recorded runs, decides
each recorded trial again from its stored signals, checks a real signature, and prints what the evidence
files say, naming what each one ran on. Python 3.9 or newer, standard library only.

### 2. Watch a swarm hire three levels deep: offline, under a second

```sh
python3 scripts/swarm-sim.py --offline
```

You see one general contractor and 24 workers with scripted strategies, no LLM. Three tasks are auctioned,
the winners auction parts of their work, and those winners do the same once more. Along the way a bidder
never reveals, a lowballer wins under its cost and cannot fund its subcontractors (`OVERCOMMITTED`), three
bidders agree to bid the maximum and get the budget and no more, a poster never awards, a winner never
delivers, and a contractor is hijacked: it tries to pay an outsider (`RECIPIENT_NOT_ALLOWED`), and the
general contractor revokes it, which cuts off the three agents it had hired. It ends with
`89/89 checks passed`. This runs on a Python model of the rules. Steps 4 and 7 run the same script
against the real contracts.

### 3. Open the live page

Open https://vigohan95.github.io/espalier/ or `site/index.html` in a browser. No server and no key: the page makes read-only view calls to a public
testnet RPC and draws the tree as the auctions grew it, with each edge labelled by its bids and price.
The slider replays the growth one award at a time, and "Before any revoke" shows the branches that were
cut. If no RPC answers it says so and shows a saved snapshot.

### 4. Check the testnet evidence: no keys, no install

```sh
python3 scripts/verify-testnet.py
python3 scripts/verify-market.py
```

The first looks up the vault's 113 recorded transactions and ends with `0 missing, 0 mismatched`. The
second looks up every transaction of the 25-agent run on testnet, reads every task and grant back through
view calls, runs the pricing rule again on the revealed bids, recomputes the invariants, and compares the
code on chain with the files in `res/`. It ends with `all checks passed`. Each takes a minute or so.
Regular RPC nodes forget a transaction after about two days, and public archival RPC allows only a few calls
a minute, so both read transactions in batches from FastNear's indexer and fall back to archival RPC for the
rest. The last run of the second is in [docs/evidence/verify-market.log](docs/evidence/verify-market.log).

| | |
|---|---|
| Vault, with the 113-transaction evidence | [`vault2.espalier-vault-1105.testnet`](https://testnet.nearblocks.io/address/vault2.espalier-vault-1105.testnet) |
| Registry | [`registry.espalier-vault-1105.testnet`](https://testnet.nearblocks.io/address/registry.espalier-vault-1105.testnet) |
| Market | [`market.espalier-swarm-1106.testnet`](https://testnet.nearblocks.io/address/market.espalier-swarm-1106.testnet) |
| Vault the market run used, same wasm | [`vault.espalier-swarm-1106.testnet`](https://testnet.nearblocks.io/address/vault.espalier-swarm-1106.testnet) |

### 5. Replay one real-model trial

```sh
python3 trials/replay.py I01
```

You see the poisoned invoice, the raw answer of the model paying the wrong account, the recipient check
decided again from stored signals (`block`, `DENYLIST_QUORUM`), and that no transaction was signed. Offline,
no model needed. `python3 trials/report.py` rebuilds the statistics from the stored files.

### 6. Run every scenario on a real local node

Needs Rust 1.95 or newer. The wasm files are prebuilt in `res/`, and near-workspaces fetches the sandbox
node, about 35 MB. If that download is slow where you are, set `NEAR_SANDBOX_BIN_PATH` to a local
`near-sandbox` binary.

```sh
cargo run --release -p harness -- sandbox --jobs 6 --report /tmp/espalier-report
```

You see one line per scenario, ending in `133/133 scenarios passed, 795 assertions`, in about seven
minutes after the build. 98 are the vault and 35 are the market. The market's auctions run in real time,
which is most of the wait: `--group market` runs only those, and `MARKET_COMMIT_MS` and `MARKET_REVEAL_MS`
lengthen the bidding windows on a slow machine. `/tmp/espalier-report.md` and `.json` hold each assertion
with expected and observed values. The committed run: [docs/evidence/sandbox-report.md](docs/evidence/sandbox-report.md).

One scenario at a time:

```sh
cargo run --release -p harness -- sandbox --only M27
```

You see `M27 pass  one revoke of a contractor ends every task below it, in the vault and in the market`.

### 7. Run the swarm and the demos on a local node

These need `NEAR_SANDBOX_BIN_PATH` set to a `near-sandbox` binary. The Python client talks to the node
itself, with no Rust involved.

```sh
python3 scripts/swarm-sim.py --sandbox   # about 12 minutes: the auctions run in real time
python3 scripts/attack-demo.py           # about a minute
python3 scripts/hire-demo.py             # about a minute
```

`swarm-sim.py --sandbox` sends every action of step 2 as a signed transaction to both contracts and ends
with `90/90 checks passed on sandbox`. The last check is that the chain ended exactly where the Python
model predicted: every task, winner, price, payment and grant.

`attack-demo.py` is the official attack demo. You see a stand-in for an injected agent refused before signing,
the same agent skipping the wrapper and landing in the queue, the owner approving it by mistake and the
contract refusing with `RECIPIENT_LISTED`, and a forged ticket refused. It ends with
`acme-b1lling.test.near received 0`. Log: [docs/evidence/attack-demo.log](docs/evidence/attack-demo.log).

`hire-demo.py` is the general contractor from the agents prompt, with one level of hiring and no auction.
You see it counter a quote once, hire on a narrower sub-budget, a rogue worker refused three ways, and
`tree spent 165 of 300 mNEAR`. It also ran on testnet: [docs/evidence/hire-demo-testnet.log](docs/evidence/hire-demo-testnet.log).

### 8. Run the property tests

The vault on the real runtime, with parallel signed transactions from 12 keys. Add `--screened` to put a
live registry in front of every spend:

```sh
cargo run --release -p harness -- sandbox-property --ops 1000 --seed 7 --keys 12
```

The vault and the market together on the real runtime: random posts, bids, awards with right and wrong
terms, reviews, draws and revokes, with the invariants checked from views after each wave:

```sh
cargo run --release -p harness -- market-property --waves 6 --seed 7
```

You see one line per wave and a JSON summary. Any broken invariant is printed with its name and the run
exits non-zero. Six waves take about 12 minutes.

The vault in process, with the same contract code on near-sdk's mock VM:

```sh
PROPERTY_SEEDS=4 PROPERTY_OPS=2500 cargo test -p espalier-vault --release -- --nocapture
```

Check that the vault's tests can fail:

```sh
python3 scripts/mutants.py
```

Each mutant breaks one line of the vault, and the property test has to fail. You see `28/28 caught`.

### 9. Read recipient signals from mainnet

```sh
python3 scripts/mainnet-signals.py wrap.near v2.ref-finance.near
```

Read-only: view calls on public RPC, no key. Public archival nodes rate-limit, so allow a few minutes per
account. It overwrites `docs/evidence/mainnet-signals.*`.

### 10. Rebuild the wasm

```sh
cargo install --locked cargo-near
(cd contract && cargo near build non-reproducible-wasm --no-abi)
(cd market && cargo near build non-reproducible-wasm --no-abi)
(cd registry && cargo near build non-reproducible-wasm --no-abi)
```

The build is not reproducible across machines yet, so the wasm you build may hash differently from `res/`.
The vault is 339 KB, the market 339 KB and the registry 171 KB.

## What was actually run

Nothing below is a claim about code that has not been executed. Contract commit and wasm sha256 are in each report.

| What | Where it ran | Result | Evidence |
|---|---|---|---|
| 133 scenarios, 795 assertions, real signed transactions, compiled wasm, no mocks. 98 vault, 35 market | near-workspaces sandbox, neard 2.14.0 | 133/133 pass | [sandbox-report.md](docs/evidence/sandbox-report.md) |
| Market: 25 scripted agents, three levels of hiring, a hijacked contractor revoked with three agents below it | NEAR testnet | 90/90 checks pass, 208 transactions, ended where the model predicted | [swarm-sim-testnet.log](docs/evidence/swarm-sim-testnet.log), [swarm-sim-testnet.json](docs/evidence/swarm-sim-testnet.json) |
| The same run | Local node | 90/90 checks pass, 208 transactions, ended where the model predicted | [swarm-sim-sandbox.log](docs/evidence/swarm-sim-sandbox.log) |
| Market and vault together: 671 random calls over 31 tasks, 1,353 invariant checks from views | **Real runtime**: sandbox, compiled wasm | No invariant broken | [market-property.json](docs/evidence/market-property.json) |
| Market feasibility probe against the unchanged vault wasm, run before any market code | Local node | 56/56 checks | [market-probe.log](docs/evidence/market-probe.log) |
| Vault: 22 scenarios (21 shared, plus T01 on the real `wrap.testnet`), 171 assertions, 113 recorded transactions | NEAR testnet | 22/22 pass | [testnet-run-txs.md](docs/evidence/testnet-run-txs.md) |
| Hire flow with a rogue worker, one level, no auction | Sandbox and NEAR testnet | 165 of 300 mNEAR spent, all rogue attempts refused | [hire-demo-testnet.log](docs/evidence/hire-demo-testnet.log) |
| Official attack demo | Sandbox | Bad address received 0 | [attack-demo.log](docs/evidence/attack-demo.log) |
| gemma3:4b (3.9B, Q4_K_M, CPU), 50 injected and 50 ordinary documents, through the real wrapper, attestor and vault | Ollama, then sandbox | See below | [trials/report.md](docs/evidence/trials/report.md) |
| Age and code-change signals on 6 real accounts, 405 archival reads | NEAR mainnet, read-only | 2 recent code changes, 1 young account | [mainnet-signals.md](docs/evidence/mainnet-signals.md) |
| Vault property test, 10,008 operations, parallel batches, revokes racing spends | **Real runtime**: sandbox, compiled wasm | All invariants held | [sandbox-property.json](docs/evidence/sandbox-property.json) |
| Same with a live registry and one listed recipient, 10,008 operations | **Real runtime** | All invariants held | [sandbox-property-screened.json](docs/evidence/sandbox-property-screened.json) |
| Vault property test, 100,000 operations, 40 seeds, independent oracle, 13 invariants | **In process, not the real runtime**: contract code on near-sdk's MockedBlockchain, with state serialised around every call and callbacks delivered in random order | All invariants held | [property-report.json](docs/evidence/property-report.json) |
| 28 mutants of the vault against the in-process property test | In process | 28/28 caught | [mutants.json](docs/evidence/mutants.json) |
| Client unit tests: RFC 8032 vectors, the market's byte layouts, the pricing vectors, the market model | Python | 28 pass | `client/tests/` |
| Market unit tests: pricing on the shared vectors and on 20,000 random bid sets, message layout | Rust | 5 pass | `market/src/` |
| Gas by depth and by subtree size | Sandbox | See below | [gas.json](docs/evidence/gas.json) |
| Toolchain | WSL Ubuntu 24.04 | | [toolchain.md](docs/evidence/toolchain.md) |

The market has no mutation test and no in-process property test: its random test runs only on the real
runtime, and at 671 calls it is much smaller than the vault's.

### Real model under indirect injection

One small model, one system prompt with no warning about injection, one attempt per document. The two
numbers are separate on purpose: how often the model was fooled, and what the guardrail did about it. This
trial is about the vault's recipient check. The market's agents are scripted.

| | |
|---|---|
| Model fooled | **38 of 50** (76%): 30 wrong recipient, 8 wrong amount |
| Refused with no human involved | **15 of 38** (39.5%) |
| Held for a human, who said no | 18 of 38. The human is scripted and perfect, so read this as "reached a person" |
| Went through | **5 of 38**: inflated amounts to real vendors, inside the limits and under 4x the history |
| Same 38 with the wrapper skipped | 34 queued for a human, 4 refused `PER_TX`, none settled |
| Ordinary payments refused outright | **0 of 48** the model got right |
| Ordinary payments sent to a human | 9 of 48, all first-time vendors |

Reason codes and the full breakdown: [docs/evidence/trials/report.md](docs/evidence/trials/report.md). Age and
code thresholds were scaled to 420 seconds, because sandbox accounts are minutes old. With 50 trials a side a
percentage moves by about 14 points at 95% confidence. A first run of the guardrail stage reported 38 of 38
stopped because of a setup flaw, and it was discarded: the report explains it.

### Measured on the sandbox

| Vault operation | Burnt gas, whole transaction |
|---|---|
| Spend at depth 0 / 4 / 8, including transfer and callback | 6.15 / 8.25 / 10.17 Tgas (about 0.5 Tgas per level) |
| Same with the registry in front | 10.72 / 13.24 / 15.58 Tgas |
| Refused by the registry, depth 0 / 4 / 8 | 7.69 / 9.68 / 11.67 Tgas |
| Largest single receipt, screened spend at depth 8 | 5.42 Tgas (the callback has 50 Tgas attached) |
| Denied spend at depth 0 / 8 | 2.53 / 3.22 Tgas |
| Revoke, subtree of 2 / 5 / 13 grants, every key deleted in the same transaction | 4.45 / 7.85 / 17.15 Tgas |
| Revoke, subtree of 31 grants: all 31 dead at once, 2 keys deleted now, 29 left to `sweep` | 11.46 Tgas |
| One `sweep` deleting those 29 keys | 35.66 Tgas |

The subtree is dead the moment `revoke` lands, at any size. Only key deletion scales, and each call does bounded work.

| Market operation | Largest burnt gas seen in the 35 scenarios, whole transaction |
|---|---|
| `post_task`: reads the poster's grant from the vault, checks the signature | 8.03 Tgas |
| `commit_bid` / `reveal_bid` (asks the vault if the key is free) | 2.48 / 7.12 Tgas |
| `close_bidding`, up to 16 bids | 5.18 Tgas |
| `confirm_award`: reads the new grant from the vault | 7.63 Tgas |
| `accept`, `claim`, `void_task` | 7.67 / 7.77 / 7.39 Tgas |
| `default_award`: three reads of the vault | 14.31 Tgas |

Each call that asks the vault needs about 50 Tgas attached, because 30 Tgas is set aside for its callback.

## How this compares with related projects at this hackathon

Read from the code in each public repository at the commit shown, on 2026-11-06. This is what the code
does, not a ranking, and they may have changed since.

| Project | What its code does | Budget above the hirer | How a hire is priced | What the hired agent holds |
|---|---|---|---|---|
| [Rootbound](https://github.com/wahab09033/rootbound) `64ca059` | A contract with a tree of authorizations for native NEAR, depth up to 3. A child must fit the parent's available budget, recipients, expiry and per-payment limit. Budgets are carved out of the parent and can be reclaimed. Revoke is a flag checked up the chain. Callers are identified by account | Yes, 4 dimensions | No auction: the parent assigns | An authorization under its own account |
| [KeyLease](https://github.com/metafrogmeme-droid/keylease) `b06721c` | Leases of API calls behind a sealed key, with sub-leases narrower in calls, expiry and endpoints, depth up to 3, and cascade revoke | Yes, for call counts, not money | No auction | A lease of calls |
| [NEXUS AiOS](https://github.com/ykbojak-alt/nexus-aios) `0f6ed1c` | An escrow contract: a manager locks a task budget, staked agents post open signed bids, the manager picks one, and releases on a result hash or slashes after the deadline. One level | No | Open bids, the manager chooses | Nothing until it is paid |
| [Swarm-for-Hire](https://github.com/yunganas/near-swarm) `a8441d2` | An escrow contract: a client locks a bounty, agents submit work, a reviewer account scores it, and passing agents split the bounty | No | No price bidding | Nothing until it is paid |
| [Swarm Bazaar](https://github.com/HATWHY4041/swarm-bazaar) `da5d4fa` | No contract. A server runs a three-round reverse auction among six agents with per-agent payment caps, and the winner passes 30% to one partner | Caps in the server | Reputation-weighted reverse auction | A funded sub-account |
| [Foreman](https://github.com/remon550/foreman) `2fc3189` | No contract. A contractor agent with an off-chain budget ledger hires on Agent Market, counters once, verifies, and pays. Payments are dry-run quotes by default | Off chain | Negotiation | |
| [Tontine](https://github.com/4n0nn43x/tontine) `1823b9a` | Sealed-bid second-price auctions in which AI firms quote swap orders, with the owner's limits checked at reveal and a bond for silence | Per firm, flat | Sealed, second price | A firm's treasury |
| [Flock](https://github.com/ClaudeOpusPremiumAgent/flock) `ff69d76` | Group buying: buyers pledge at a ceiling, sellers bid sealed with a bond, the lowest wins at the second-lowest price. One level | No: each agent has its own balance | Sealed, second price | Goods |
| **Espalier** | Vault and market, as above | Yes: 14 dimensions, every draw charged to every level | Sealed, second price | A grant under the hirer's, with a key the vault issued, from which it can hire again |

What Espalier does that we did not find in those: hiring that repeats down a chain with each level bounded
by the ones above, a market whose prize is itself a delegated budget, and one revoke that ends a contractor
and everyone it hired. What several of them have and Espalier does not: mainnet deployments, NEAR Intents
settlement, agents driven by a hosted model with TEE attestation, an interactive web app, and contracts already
locked.

## Where to check each of the five judging criteria

| Criterion | Start here |
|---|---|
| Technical implementation | `python3 scripts/story.py`, then [docs/ATTACKS.md](docs/ATTACKS.md): 134 numbered scenarios against 56 threats (133 on a local node, plus T01, which needs the real `wrap.testnet` and ran only on testnet). `contract/src/lib.rs` and `market/src/lib.rs` are the two contracts |
| Design | https://vigohan95.github.io/espalier/, the live read-only page. [docs/SWARM-RULES.md](docs/SWARM-RULES.md) section 3 has one task as a text sequence diagram, and section 7 says which contract enforces what |
| Impact | The problem statement above. [docs/BASELINE-GAPS.md](docs/BASELINE-GAPS.md) maps limits that the official reference products document to tests here. The injection trial reports what got through as well as what was stopped |
| Originality | The comparison table above, and [docs/SWARM-RULES.md](docs/SWARM-RULES.md) section 9: why this needs a delegation tree and not a wallet per agent |
| Sustainability | Two contracts with no admin and no operator: anyone can deploy them and nothing has to keep running. MIT license. Every claim has a command that rechecks it, and CI runs them. "What remains" below is the honest list |

## Key limits, as issued to an agent

The vault adds each agent's key to its own account with `AddKey`. The protocol then enforces:

- Receiver: only the vault (B08 shows `ReceiverMismatch`).
- Methods: only the ones in the grant, never a callback (B07 and B09 show `MethodNameMismatch`).
- No attached deposit, so the key cannot send NEAR on its own (B08 shows `DepositWithFunctionCall`).
- It cannot add keys or send a transfer: those need a full-access key (B08).
- Gas comes out of the key's allowance. The whole prepaid gas is charged up front and the unused part comes
  back a few blocks later, so a screened spend at 120 Tgas needs about 121 mNEAR of allowance at that moment,
  while it costs about 1 mNEAR in the end. When the allowance cannot cover a call the protocol refuses it. That
  is a lockout, and the way out is `refill_key` from an ancestor or the owner (F03).

A grant key cannot send a transaction to the market either. A poster signs its messages with that key, and
any account carries them: the market checks the signature against the key the vault reports.

The testnet contracts still hold their deployment keys, because the code may still change.
`harness testnet --phase lock` deletes the vault's, after which the code can never change. That has not been
run yet, so `verify-testnet.py` reports `1 full-access`. The first vault, `vault.espalier-vault-1105.testnet`,
ran the milestone 1 contract, whose state layout this one cannot read. Its code was swapped for a smaller
contract to free its storage (`scripts/testnet-retire-v1.py`), and the vault evidence is on `vault2`.

## Threat model

Trust assumptions: the owner account is honest and its key is safe, and the NEAR protocol works as specified.
Any agent key can be stolen, and any agent can be prompt-injected. In the market the attacker can also be a
worker, a poster, or whoever carries a message.
[docs/ATTACKS.md](docs/ATTACKS.md) maps 56 threats to 134 numbered scenarios, each with its result, and is generated
from the evidence reports. That is the 133 local scenarios plus T01, which ran only on testnet.

### What it does not stop

- **A bad payment inside the limits.** The trials show it: an inflated invoice from a real vendor went through 5
  times out of 5. The check is about the recipient, not about whether the amount matches the invoice.
- **A poster that refuses good work.** A poster can reject a delivery, or revoke the winner before it draws.
  The contract cannot judge work. It counts rejections and defaults per poster, where a worker can read them.
- **A contractor that starves its own subcontractor.** If it draws its cap down first, the vault refuses the
  subcontractor's draw at the contractor's grant (M32). The tree's bound holds, and the subcontractor is short.
- **Work cut off with its contractor.** When a contractor is revoked, agents below it that were not yet paid
  are not paid. Their bonds come back.
- **Collusion among approved workers.** A cartel can lift a price to the task's maximum budget, and a hijacked
  poster with an accomplice on the owner's roster can run a sham task up to the poster's own cap.
- **Returned money does not restore budget.** When a winner draws without being accepted, the market sends
  the money back to the vault, but the vault has no method that lowers the counters again. The tree is then
  charged more than was paid. This happens only when a winner misbehaves, and is bounded by that winner's cap.
- **A human fooled by the same text.** A `warn` only moves the decision to a person.
- **Stolen attestor key.** An attacker gets tickets, and then the limits, the allowlist and the denylist remain.
  There is no TEE here, and the contract cannot confirm what the attestor looked at.
- **Code changing after a ticket.** A ticket lasts minutes, and the contract cannot see a recipient's code.
- **Two colluding trusted reporters** can block a recipient, until the owner drops them.
- **Code that follows a publisher.** An account on a global contract by publisher account can change code with no
  trace on the account, so the check only warns about that.
- **A stolen owner key.** The owner can create roots, approve anything and withdraw.
- **Malformed calls (F04).** Arguments that fail to parse panic before contract code runs, so they burn gas
  and do not move the denial counter. Only the key's allowance bounds them.
- **A stuck reservation.** If a settlement callback ever failed outright, its amount would stay reserved.
  That is safe, since nothing is double-spent, but the budget stays consumed. No test has produced this.
- **Storage is never reclaimed.** Revoked grants stay as tombstones, because the walk needs them. Contact records
  and the market's finished tasks stay too.
- **Malicious accepted tokens.** The owner chooses the tokens. A token that lies about a transfer breaks
  that token's ledger, not other assets. The market handles native NEAR only.

The market's full list is in [docs/SWARM-RULES.md](docs/SWARM-RULES.md) section 10.

## Layout

| Path | What |
|---|---|
| `contract/src/lib.rs` | The vault |
| `contract/src/window.rs` | Rolling window, with its own tests |
| `contract/src/property.rs` | In-process property test and oracle |
| `market/src/lib.rs` | The market |
| `market/src/pricing.rs`, `market/fixtures/pricing-vectors.json` | The pricing rule and the vectors shared by Rust, Python and the page |
| `registry/src/lib.rs` | Shared denylist |
| `client/espalier/` | Python client, standard library only: `wrapper.py`, `signals.py`, `policy.py`, `attestor.py`, `near.py`, and for the market `market.py`, `market_model.py`, `snapshot.py` |
| `trials/` | Corpus, model stage, guardrail stage, report, replay |
| `harness/src/scenarios.rs`, `recipient.rs`, `market.rs` | The scenarios |
| `harness/src/property.rs`, `market_property.rs` | Real-runtime property tests |
| `harness/src/testnet.rs` | Testnet deploy, run and lock for the vault |
| `scripts/swarm-sim.py`, `verify-market.py` | The 25-agent run in its three modes, and its read-only verifier |
| `site/` | The live page |
| `test-contracts/` | Attacker tools: a NEP-141 token with switches, a relay |
| `res/` | Prebuilt wasm |
| `scripts/` | Story, preflight, demos, verifiers, evidence regeneration. The README commands do not need the `.sh` files |
| `docs/submission/` | Drafts of the X post, video script and submission fields |

Every refusal carries a reason code. The vault's are NEP-297 events, `standard: "espalier"`, with the numbers
used, such as `26000000000000000000000 > 25000000000000000000000 per-tx` (G01). The market's are
`standard: "espalier-market"`.

## What remains

Not built: a reproducible wasm build, NEAR Intents, an external audit, integration with market.near.ai, which
needs an account or mainnet USDC, a mutation test for the market, agents in the market driven by a real model.
The testnet contracts are not locked yet.

## Hackathon window, AI assistance and libraries

All code, tests and documents in this repository were written by Claude Code (Claude Opus 5.5 for the vault,
Claude Fable 5.1 and Claude Opus 5.5 for the market) under human direction, starting on 2026-11-05 inside the hackathon window.
The first commit is in the window, so there is no earlier baseline to separate, and the `pre-hackathon` tag of
rules Q-003 does not apply.

Libraries: near-sdk 5.30.0, near-contract-standards 5.30.0 (test token only), near-workspaces 0.24.0, tokio,
serde, serde_json, anyhow, futures, rand, ed25519-dalek, bs58, sha2. The Python client and the live page use
no library. Tools: cargo-near 0.22.0, near-cli-rs 0.30.1, neard sandbox 2.14.0, Ollama 0.40.13 with gemma3:4b.

## License

MIT. See [LICENSE](LICENSE).
