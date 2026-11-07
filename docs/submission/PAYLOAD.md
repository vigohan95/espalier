# Submission payload draft

Draft only. Nothing has been sent. The rules allow one submission per repository, and the payload must be
confirmed by the human before it goes out. A mistake afterwards goes through https://nearhackathon.com/request-changes,
never through a second submission. The rules also say not to commit the final payload, because it
holds personal data, so the personal fields stay out of this file.

Endpoint: `POST https://nearhackathon.com/api/applications`, from 2026-11-01 to 2026-11-14 23:59 UTC.

| Field | Value | Status |
|---|---|---|
| requestId | Generate once at submission, and reuse it on a retry | At submission |
| projectName | `Espalier` | Draft |
| leadName | | **Human confirms** the value in the private notes |
| email | | **Human confirms** the value in the private notes |
| size | `1` | Human confirms |
| tracks | `["swarms", "security", "agents"]` | Human confirms |
| nearAddress | | **Human confirms** the value in the private notes: mainnet account for the payout |
| projectUrl | `https://github.com/vigohan95/espalier` | Valid only once the repository is public, which needs the human's confirmation. It must be public by the deadline |
| demoUrl | | After the X post is up |
| idea | Below, 1040 characters of the 2,000 allowed | Draft |
| consent | `true`, only after the human agrees to the notice | **Human decides** |

Do not add other fields. `website` is a spam trap.

Consent notice, to be read to the human word for word: "I agree that the organizers may use these details to
review our submission and contact us about this hackathon."

## idea

```text
When an AI agent with a budget hires other agents, it either shares its key or hands out wallets that nothing ties back to the original limit, and neither survives a second level of hiring. Espalier makes each hire a sealed-bid auction on NEAR. The lowest bid wins at the second-lowest price, and the prize is not money in escrow: it is a grant in a vault, a child of the hirer's budget, capped at the price and narrower in all 14 dimensions. The winner can hire from that grant in turn, so auctions grow the budget tree one node at a time. Every draw is charged to every level above it, the vault issues and deletes each agent's key, and one revoke cuts off a contractor and everyone it hired.

The market contract only reads the vault, whose wasm did not change for it. A scripted run of 25 agents on testnet: 13 auctions, 12 grants, 208 transactions, a hijacked contractor refused and revoked with 3 agents below it, and the chain ended where an independent model predicted. 133 scenarios pass on a real local node. Testnet, not audited.
```
