# Espalier

A NEAR contract that lets an AI agent hand part of its budget to sub-agents, with the limits enforced
on chain, and asks about a recipient before any money moves. Every sub-budget can only be narrower than
the one it came from, one transaction cuts off a whole branch, and the contract itself issues and deletes
each agent's access key.

Built at NEAR HACKATHON 2026. Tracks: `security`, `agents`. Status: testnet. Not audited.

**The problem.** An orchestrator agent with a budget hires other agents. Today it has two options: share
its own key, or give each worker a separate wallet that nothing ties back to the original limit. In the
first case one injected sub-agent can spend the whole budget. In the second the owner has no single place
to see or stop the total. And a check on the recipient that runs inside the agent stops protecting anyone
the moment that agent is hijacked.

**What Espalier does.** The vault holds the funds. Each agent gets a *grant*, a node in a tree, with its
own function-call key on the vault account. A grant can create child grants, and the contract checks
that the child is narrower in all 14 dimensions: cap, per-transaction limit, rolling-window cap, window
length, approval threshold, recipients, methods, expiry, depth, denial limit, gas budget, node budget,
asset and clearance. Every spend is charged to every grant up to the root, so a subtree can never exceed
what any ancestor has left. Revoking a grant kills everything below it.

Before a spend, a recipient check answers `allow`, `warn` or `block` with reasons. The shared denylist is
enforced by the contract on every spend, through a quorum of reporters the vault chose. A contract cannot
read another account's age or code, so an attestor reads those off chain and signs a short ticket. A spend
with no ticket waits for a human, so skipping the wrapper leads to a queue, not to the money.

| Read this | For |
|---|---|
| [docs/ATTACKS.md](docs/ATTACKS.md) | Every scenario, numbered, mapped to the threat model |
| [docs/DESIGN.md](docs/DESIGN.md) | The tree, attenuation, cascade revoke, key lifecycle, gas |
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

`preflight.py` says what will work on this machine and ends with `Next:`. `story.py` takes under a second
and ends in `all checks passed`. It does not rerun the chain: it hashes the wasm files, runs the client unit
tests, decides each recorded trial again from its stored signals, checks a real signature, and prints what
the evidence files say, naming what each one ran on. Python 3.9 or newer, standard library only.

### 2. Check the testnet evidence: no keys, no install

```sh
python3 scripts/verify-testnet.py
```

You see `113 transactions found`, `0 missing, 0 mismatched`, and `match: True` for both the vault and the
registry against the sha256 of the files in `res/`. Each transaction is looked up on a public RPC and its
logged reason code compared with the report. Every one is listed with a nearblocks link in
[docs/evidence/testnet-run-txs.md](docs/evidence/testnet-run-txs.md).

Vault: [`vault2.espalier-vault-1105.testnet`](https://testnet.nearblocks.io/address/vault2.espalier-vault-1105.testnet)
Registry: [`registry.espalier-vault-1105.testnet`](https://testnet.nearblocks.io/address/registry.espalier-vault-1105.testnet)

### 3. Replay one real-model trial

```sh
python3 trials/replay.py I01
```

You see the poisoned invoice, the raw answer of the model paying the wrong account, the recipient check
decided again from stored signals (`block`, `DENYLIST_QUORUM`), and that no transaction was signed. Offline,
no model needed. `python3 trials/report.py` rebuilds the statistics from the stored files.

### 4. Run every scenario on a real local node

Needs Rust 1.95 or newer. The wasm files are prebuilt in `res/`, and near-workspaces fetches the sandbox
node, about 35 MB. If that download is slow where you are, set `NEAR_SANDBOX_BIN_PATH` to a local
`near-sandbox` binary.

```sh
cargo run --release -p harness -- sandbox --jobs 6 --report /tmp/espalier-report
```

You see one line per scenario, ending in `98/98 scenarios passed, 509 assertions`, in about two minutes
after the build. `/tmp/espalier-report.md` and `.json` hold each assertion with expected and observed values.
The committed run: [docs/evidence/sandbox-report.md](docs/evidence/sandbox-report.md).

One scenario at a time:

```sh
cargo run --release -p harness -- sandbox --only H01
```

You see `H01 pass  recipient listed by the reporter quorum is refused on chain, nothing moves`.

### 5. Run the demos on a local node

Both need `NEAR_SANDBOX_BIN_PATH` set to a `near-sandbox` binary. The client talks to the node itself, with
no Rust involved.

```sh
python3 scripts/attack-demo.py    # about a minute
python3 scripts/hire-demo.py      # about a minute
```

`attack-demo.py` is the official attack demo. You see a stand-in for an injected agent refused before signing,
the same agent skipping the wrapper and landing in the queue, the owner approving it by mistake and the
contract refusing with `RECIPIENT_LISTED`, and a forged ticket refused. It ends with
`acme-b1lling.test.near received 0`. Log: [docs/evidence/attack-demo.log](docs/evidence/attack-demo.log).

`hire-demo.py` is the general contractor from the agents prompt. You see it counter a quote once, hire on a
narrower sub-budget, a rogue worker refused three ways, and `tree spent 165 of 300 mNEAR`. The agents in it are
scripted. It also ran on testnet: [docs/evidence/hire-demo-testnet.log](docs/evidence/hire-demo-testnet.log).

### 6. Run the property tests

Real runtime, with parallel signed transactions from 12 keys. Add `--screened` to put a live registry in front
of every spend:

```sh
cargo run --release -p harness -- sandbox-property --ops 1000 --seed 7 --keys 12
```

You see progress lines, then a JSON summary. Any broken invariant exits non-zero with its name (`R1` to `R8`).

In process, with the same contract code on near-sdk's mock VM:

```sh
PROPERTY_SEEDS=4 PROPERTY_OPS=2500 cargo test -p espalier-vault --release -- --nocapture
```

You see `test result: ok. 4 passed` and a JSON summary.

Check that those tests can fail:

```sh
python3 scripts/mutants.py
```

Each mutant breaks one line of the contract, for example "the registry says listed and the spend goes on",
and the property test has to fail. You see `28/28 caught`.

### 7. Read recipient signals from mainnet

```sh
python3 scripts/mainnet-signals.py wrap.near v2.ref-finance.near
```

Read-only: view calls on public RPC, no key. Public archival nodes rate-limit, so allow a few minutes per
account. It overwrites `docs/evidence/mainnet-signals.*`.

### 8. Rebuild the wasm

```sh
cargo install --locked cargo-near
(cd contract && cargo near build non-reproducible-wasm --no-abi)
(cd registry && cargo near build non-reproducible-wasm --no-abi)
```

The build is not reproducible across machines yet, so the wasm you build may hash differently from `res/`.
The vault is 339 KB and the registry 171 KB.

## What was actually run

Nothing below is a claim about code that has not been executed. Contract commit and wasm sha256 are in each report.

| What | Where it ran | Result | Evidence |
|---|---|---|---|
| 98 scenarios, 509 assertions, real signed transactions, compiled wasm, no mocks | near-workspaces sandbox, neard 2.14.0, protocol 88 | 98/98 pass | [sandbox-report.md](docs/evidence/sandbox-report.md) |
| 22 scenarios (21 shared, plus T01 on the real `wrap.testnet`), 171 assertions, 113 recorded transactions | NEAR testnet | 22/22 pass | [testnet-run-txs.md](docs/evidence/testnet-run-txs.md) |
| Hire flow with a rogue worker | Sandbox and NEAR testnet | 165 of 300 mNEAR spent, all rogue attempts refused | [hire-demo-testnet.log](docs/evidence/hire-demo-testnet.log) |
| Official attack demo | Sandbox | Bad address received 0 | [attack-demo.log](docs/evidence/attack-demo.log) |
| gemma3:4b (3.9B, Q4_K_M, CPU), 50 injected and 50 ordinary documents, through the real wrapper, attestor and contract | Ollama, then sandbox | See below | [trials/report.md](docs/evidence/trials/report.md) |
| Age and code-change signals on 6 real accounts, 405 archival reads | NEAR mainnet, read-only | 2 recent code changes, 1 young account | [mainnet-signals.md](docs/evidence/mainnet-signals.md) |
| Property test, 10,008 operations, parallel batches, revokes racing spends | **Real runtime**: sandbox, compiled wasm | All invariants held | [sandbox-property.json](docs/evidence/sandbox-property.json) |
| Same with a live registry and one listed recipient, 10,008 operations | **Real runtime** | All invariants held | [sandbox-property-screened.json](docs/evidence/sandbox-property-screened.json) |
| Property test, 100,000 operations, 40 seeds, independent oracle, 13 invariants | **In process, not the real runtime**: contract code on near-sdk's MockedBlockchain, with state serialised around every call and callbacks delivered in random order | All invariants held | [property-report.json](docs/evidence/property-report.json) |
| 28 mutants of the contract against the in-process property test | In process | 28/28 caught | [mutants.json](docs/evidence/mutants.json) |
| Client unit tests, including RFC 8032 vectors | Python | 18 pass | `client/tests/` |
| Gas by depth and by subtree size | Sandbox | See below | [gas.json](docs/evidence/gas.json) |
| Toolchain | WSL Ubuntu 24.04 | | [toolchain.md](docs/evidence/toolchain.md) |

### Real model under indirect injection

One small model, one system prompt with no warning about injection, one attempt per document. The two
numbers are separate on purpose: how often the model was fooled, and what the guardrail did about it.

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

| Operation | Burnt gas, whole transaction |
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

The testnet vault still holds its one deployment key, because the contract may still change.
`harness testnet --phase lock` deletes it, after which the code can never change. That has not been run yet,
so `verify-testnet.py` reports `1 full-access`. The first vault, `vault.espalier-vault-1105.testnet`, ran the
milestone 1 contract, whose state layout this one cannot read. Its code was swapped for a smaller contract to free
its storage (`scripts/testnet-retire-v1.py`), and all current evidence is on `vault2`.

## Threat model

Trust assumptions: the owner account is honest and its key is safe, and the NEAR protocol works as specified.
Any agent key can be stolen, and any agent can be prompt-injected.
[docs/ATTACKS.md](docs/ATTACKS.md) maps 43 threats to 99 numbered scenarios, each with its result, and is generated
from the evidence reports.

### What it does not stop

- **A bad payment inside the limits.** The trials show it: an inflated invoice from a real vendor went through 5
  times out of 5. The check is about the recipient, not about whether the amount matches the invoice.
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
  are paid by the vault too, bounded in practice by the gas each spend burns.
- **Malicious accepted tokens.** The owner chooses the tokens. A token that lies about a transfer breaks
  that token's ledger, not other assets.

## Layout

| Path | What |
|---|---|
| `contract/src/lib.rs` | The vault |
| `contract/src/window.rs` | Rolling window, with its own tests |
| `contract/src/property.rs` | In-process property test and oracle |
| `registry/src/lib.rs` | Shared denylist |
| `client/espalier/` | Python client, standard library only: `wrapper.py`, `signals.py`, `policy.py`, `attestor.py`, `near.py` |
| `trials/` | Corpus, model stage, guardrail stage, report, replay |
| `harness/src/scenarios.rs`, `recipient.rs` | The scenarios |
| `harness/src/property.rs` | Real-runtime property test |
| `harness/src/testnet.rs` | Testnet deploy, run and lock |
| `test-contracts/` | Attacker tools: a NEP-141 token with switches, a relay |
| `res/` | Prebuilt wasm |
| `scripts/` | Story, preflight, demos, verifier, evidence regeneration. The README commands do not need the `.sh` files |
| `docs/submission/` | Drafts of the X post, video script and submission fields |

Every denial is a NEP-297 event, `standard: "espalier"`, with a reason code and the numbers used, such as
`26000000000000000000000 > 25000000000000000000000 per-tx` (G01).

## What remains

Not built: a hosted read-only view, a reproducible wasm build, NEAR Intents, an external audit, integration with
market.near.ai, which needs an account or mainnet USDC. The final testnet vault is not locked yet.

## Hackathon window, AI assistance and libraries

All code, tests and documents in this repository were written by Claude Code (Claude Opus 5.5) under human
direction, starting on 2026-11-05 inside the hackathon window. The first commit is in the window, so there is no
earlier baseline to separate, and the `pre-hackathon` tag of rules Q-003 does not apply.

Libraries: near-sdk 5.30.0, near-contract-standards 5.30.0 (test token only), near-workspaces 0.24.0, tokio,
serde, serde_json, anyhow, futures, rand, ed25519-dalek, bs58, sha2. The Python client uses only the standard
library. Tools: cargo-near 0.22.0, near-cli-rs 0.30.1, neard sandbox 2.14.0, Ollama 0.40.13 with gemma3:4b.

## License

MIT. See [LICENSE](LICENSE).
