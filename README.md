# Espalier

A NEAR contract that lets an AI agent hand part of its budget to sub-agents, with the limits enforced
on chain. Every sub-budget can only be narrower than the one it came from, one transaction cuts off a
whole branch, and the contract itself issues and deletes each agent's access key.

Built at NEAR HACKATHON 2026. Tracks: `security`, `agents`. Status: testnet, milestone 1. Not audited.

**The problem.** An orchestrator agent with a budget hires other agents. Today it has two options: share
its own key, or give each worker a separate wallet that nothing ties back to the original limit. In the
first case one injected sub-agent can spend the whole budget. In the second the owner has no single place
to see or stop the total.

**What Espalier does.** The vault holds the funds. Each agent gets a *grant*, a node in a tree, with its
own function-call key on the vault account. A grant can create child grants, and the contract checks
that the child is narrower in all 13 dimensions: cap, per-transaction limit, rolling-window cap, window
length, approval threshold, recipients, methods, expiry, depth, denial limit, gas budget, node budget and
asset. Every spend is charged to every grant up to the root, so a subtree can never exceed what any
ancestor has left. Revoking a grant kills everything below it.

How it works, and why the gas of a revoke does not depend on subtree size: [docs/DESIGN.md](docs/DESIGN.md).

## Try it

### 1. Check the testnet evidence: no keys, no install

```sh
python3 scripts/verify-testnet.py
```

You see `72 transactions found`, `0 missing, 0 mismatched`, and `match: True` for the on-chain code
hash against the sha256 of `res/espalier_vault.wasm`. The check looks up each transaction and compares
its logged reason code with the report. It uses only the Python standard library and a public RPC. Every transaction is listed with a nearblocks link in
[docs/evidence/testnet-run-txs.md](docs/evidence/testnet-run-txs.md).

Vault: [`vault.espalier-vault-1105.testnet`](https://testnet.nearblocks.io/address/vault.espalier-vault-1105.testnet)

### 2. Run every scenario on a real local node

Needs Rust 1.95 or newer. The wasm files are prebuilt in `res/`, and near-workspaces fetches the sandbox
node, about 35 MB. If that download is slow where you are, set `NEAR_SANDBOX_BIN_PATH` to a local
`near-sandbox` binary.

```sh
cargo run --release -p harness -- sandbox --jobs 6 --report /tmp/espalier-report
```

You see one line per scenario, ending in `71/71 scenarios passed, 328 assertions`, in about two
minutes after the build. `/tmp/espalier-report.md` and `.json` hold each assertion with expected and
observed values. The committed run: [docs/evidence/sandbox-report.md](docs/evidence/sandbox-report.md).

One scenario at a time, for example the depth-6 cascade:

```sh
cargo run --release -p harness -- sandbox --only C01
```

You see `C01 pass  one transaction revokes a depth-6 chain and deletes its keys`.

### 3. Run the property tests

Real runtime, with parallel signed transactions from 12 keys:

```sh
cargo run --release -p harness -- sandbox-property --ops 1000 --seed 7 --keys 12
```

You see progress lines, then a JSON summary. Any broken invariant exits non-zero with its name (`R1` to
`R7`). The committed run is 10,008 operations in 17 rounds, with 33 revokes racing live batches: [docs/evidence/sandbox-property.json](docs/evidence/sandbox-property.json).

In process, with the same contract code on near-sdk's mock VM:

```sh
PROPERTY_SEEDS=4 PROPERTY_OPS=2500 cargo test -p espalier-vault --release -- --nocapture
```

You see `test result: ok. 4 passed` and a JSON summary. The committed run is 100,000 operations:
[docs/evidence/property-report.json](docs/evidence/property-report.json).

Check that those tests can fail:

```sh
python3 scripts/mutants.py
```

Each of 18 mutants breaks one line of the contract, for example "the cap check stops at the leaf", and the
property test has to fail. You see `18/18 caught`: [docs/evidence/mutants.json](docs/evidence/mutants.json).

### 4. Rebuild the wasm

```sh
cargo install --locked cargo-near
(cd contract && cargo near build non-reproducible-wasm --no-abi)
```

The build is not reproducible across machines yet, so the wasm you build may hash differently from
`res/espalier_vault.wasm`. The contract is 295 KB.

## What was actually run

Nothing below is a claim about code that has not been executed. Contract commit and wasm sha256 are in each report.

| What | Where it ran | Result | Evidence |
|---|---|---|---|
| 71 scenarios, 328 assertions, real signed transactions, compiled wasm, no mocks | near-workspaces sandbox, neard 2.14.0, protocol 88 | 71/71 pass | [sandbox-report.md](docs/evidence/sandbox-report.md) |
| 16 of those scenarios (15 shared, plus T01 on the real `wrap.testnet`), 116 assertions, 72 recorded transactions | NEAR testnet | 16/16 pass | [testnet-run-txs.md](docs/evidence/testnet-run-txs.md) |
| Property test, 10,008 operations, parallel batches, revokes racing spends | **Real runtime**: sandbox, compiled wasm | All invariants held | [sandbox-property.json](docs/evidence/sandbox-property.json) |
| Property test, 100,000 operations, 40 seeds, independent oracle | **In process, not the real runtime**: contract code on near-sdk's MockedBlockchain, with state serialised around every call and callbacks delivered in random order | All invariants held | [property-report.json](docs/evidence/property-report.json) |
| 18 mutants of the contract against the in-process property test | In process | 18/18 caught | [mutants.json](docs/evidence/mutants.json) |
| Gas by depth and by subtree size | Sandbox | See below | [gas.json](docs/evidence/gas.json) |
| Toolchain | WSL Ubuntu 24.04 | | [toolchain.md](docs/evidence/toolchain.md) |

Measured on the sandbox:

| Operation | Burnt gas, whole transaction |
|---|---|
| Spend at depth 0 / 4 / 8, including transfer and callback | 5.88 / 7.80 / 9.71 Tgas (about 0.48 Tgas per level) |
| Largest single receipt in a spend at depth 8 | 5.12 Tgas (the callback has 20 Tgas attached) |
| Denied spend at depth 0 / 8 | 2.47 / 3.17 Tgas |
| Revoke, subtree of 2 / 5 / 13 grants, every key deleted in the same transaction | 4.36 / 7.72 / 16.72 Tgas |
| Revoke, subtree of 31 grants: all 31 dead at once, 2 keys deleted now, 29 left to `sweep` | 11.23 Tgas |
| One `sweep` deleting those 29 keys | 35.20 Tgas |

The subtree is dead the moment `revoke` lands, at any size. Only key deletion scales, and each call does bounded work.

## Key limits, as issued to an agent

The vault adds each agent's key to its own account with `AddKey`. The protocol then enforces:

- Receiver: only the vault (B08 shows `ReceiverMismatch`).
- Methods: only the ones in the grant, never a callback (B07 and B09 show `MethodNameMismatch`).
- No attached deposit, so the key cannot send NEAR on its own (B08 shows `DepositWithFunctionCall`).
- It cannot add keys or send a transfer: those need a full-access key (B08).
- Gas comes out of the key's allowance. When the allowance cannot cover a call the protocol refuses it.
  That is a lockout, and the way out is `refill_key` from an ancestor or the owner (F03).

The testnet vault still holds its one deployment key, because the contract will change in later
milestones. `harness testnet --phase lock` deletes it, after which the code can never change. That
has not been run yet, so `verify-testnet.py` reports `1 full-access`.

## Threat model

Trust assumptions: the owner account is honest and its key is safe, and the NEAR protocol works as
specified. Any agent key can be stolen, and any agent can be prompt-injected. In every row below the
attacker holds a real agent key unless it says otherwise. ✓ means the scenario also ran on testnet.

| Threat | What stops it | Enforced by | Scenarios |
|---|---|---|---|
| Sub-agent asks for more than its parent has, in any dimension | Attenuation check at `delegate`, one code per dimension | Contract | A01✓ to A14, A16 |
| Siblings together overspend a shared parent | Every spend is charged to every ancestor | Contract | A17✓, E06 |
| Leaf has room but a mid-tree ancestor does not | The walk checks each level | Contract | A18 |
| Empty method list on a key, which means every method | Refused at `delegate` | Contract | A15 |
| Key calls a method outside its grant | Method list is on the key | Protocol | B09 |
| Child revokes its parent or a sibling | Only strict ancestors can revoke | Contract | B01✓, B03 |
| Child approves its own spend | Only strict ancestors can approve | Contract | B02 |
| A contract relays a call into the vault | `predecessor` must be the vault | Contract | B04 |
| The same key registered on another account | `signer` must be the vault | Contract | B05 |
| Malicious token re-enters during a spend, with the agent's real key as signer | `predecessor` must be the vault | Contract | B06 |
| Agent calls a settlement callback directly | Callback names are on no issued key | Protocol | B07✓ |
| Key tries to move NEAR, add keys, attach deposit or call another contract | Function-call key limits | Protocol | B08✓ |
| Stranger uses owner methods | Owner check | Contract | B10 |
| Owner set to the vault, making every agent the owner | Refused at `new` | Contract | B11 |
| Revoked subtree keeps spending | Lineage walk, then keys deleted | Contract and protocol | C01✓, C02✓, C03 |
| Spend in flight during a revoke | Settles or is refused, and the counters match the money | Contract | C06 |
| Revoked grant still has an approved spend | Lineage re-checked at execution | Contract | C05, D09 |
| Expired grant | Lineage walk; the attempt deletes the key | Contract | C08 |
| Approval replayed | Entry removed before funds move | Contract | D02✓ |
| Spend swapped after approval | Arguments are re-hashed at execution | Contract | D03, D04 |
| Policy changed or budget gone after approval | Every limit is evaluated again | Contract | D05, D06 |
| Agent approves above its authority | Approver's own threshold, then the owner | Contract | D07 |
| Approval queue flooded | 8 per grant | Contract | D10 |
| Failed transfer leaks budget | Callback releases every level and the window | Contract | E03✓, E04, T01✓ |
| Concurrent receipts race past a limit | Reservation at every level, inside one receipt | Contract and runtime | E06, E07 |
| Payment drains the balance needed for storage | Check before any state change | Contract | E05 |
| Reset boundary allows 2x the window | Rolling window, no boundary | Contract | E08 |
| Payment split to get under a limit | Rolling window still counts it | Contract | E11 |
| Too little gas strands a reservation | Refused and counted before reserving | Contract | E09 |
| Unknown token deposited | Returned to the sender | Contract | E10 |
| Compromised key burns the vault's gas with denied calls | N denials revoke it, and the allowance is a hard stop | Contract and protocol | F01✓, F02 |
| Denial counter rolled back by a panic | Denials return normally | Contract | F08 |
| Agent locked out by an exhausted allowance | Ancestor refills, charged to its own budget | Contract | F03 |
| Key leaked but grant still wanted | Rotation in one receipt | Contract | F05 |
| Child key collides with a key already on the vault | `AddKey` failure buries the grant and refunds the parent | Contract | F06, F09 |

### What it does not stop

- **A bad payment inside the limits.** An injected agent can still pay an allowlisted recipient up to its
  caps. Espalier bounds the damage and does not judge intent. There is no recipient risk check yet.
- **A stolen owner key.** The owner can create roots, approve anything and withdraw.
- **Malformed calls (F04).** Arguments that fail to parse panic before contract code runs, so they burn gas
  and do not move the denial counter. Only the key's allowance bounds them.
- **A stuck reservation.** If a settlement callback ever failed outright, its amount would stay reserved.
  That is safe, since nothing is double-spent, but the budget stays consumed. No test has produced this.
- **Storage is never reclaimed.** Revoked grants stay as tombstones, because the walk needs them.
- **Malicious accepted tokens.** The owner chooses the tokens. A token that lies about a transfer breaks
  that token's ledger, not other assets.

## Layout

| Path | What |
|---|---|
| `contract/src/lib.rs` | The vault |
| `contract/src/window.rs` | Rolling window, with its own tests |
| `contract/src/property.rs` | In-process property test and oracle |
| `harness/src/scenarios.rs` | The 71 scenarios |
| `harness/src/property.rs` | Real-runtime property test |
| `harness/src/testnet.rs` | Testnet deploy, run and lock |
| `test-contracts/` | Attacker tools: a NEP-141 token with switches, a relay |
| `res/` | Prebuilt wasm |
| `scripts/` | Toolchain install, evidence regeneration, read-only verifier. The README commands do not need the `.sh` files |

Every denial is a NEP-297 event, `standard: "espalier"`, with a reason code and the numbers used, such as
`26000000000000000000000 > 25000000000000000000000 per-tx` (G01).

## What remains

This is milestone 1. Not built yet:

- Recipient check returning allow, warn or block, using account age, contract status, recent code change,
  history and a shared blocklist, plus the agent-side wrapper that refuses to sign on block.
- A real LLM agent under indirect injection, with measured block and false-block rates.
- Prompt-injection demo script and video, and the comparison against the documented gaps in the
  official reference products.
- `llms.txt`, a one-screen offline overview command and a hosted read-only view.
- Locking the final testnet vault. Reproducible wasm build. External audit.

## AI assistance and libraries

All code, tests and documents in this repository were written by Claude Code (Claude Opus 5.5) under human
direction, starting on 2026-11-05 inside the hackathon window. There is no earlier baseline.

Libraries: near-sdk 5.30.0, near-contract-standards 5.30.0 (test token only), near-workspaces 0.24.0,
tokio, serde, serde_json, anyhow, futures, rand. Tools: cargo-near 0.22.0, near-cli-rs 0.30.1, neard sandbox 2.14.0.

## License

MIT. See [LICENSE](LICENSE).
