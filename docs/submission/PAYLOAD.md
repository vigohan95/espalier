# Submission payload draft

Draft only. Nothing has been sent. The rules allow one submission per repository, and the payload must be
confirmed by the human before it goes out. The rules also say not to commit the final payload, because it
holds personal data, so the personal fields stay out of this file.

Endpoint: `POST https://nearhackathon.com/api/applications`, from 2026-11-01 to 2026-11-14 23:59 UTC.

| Field | Value | Status |
|---|---|---|
| requestId | Generate once at submission, and reuse it on a retry | At submission |
| projectName | `Espalier` | Draft |
| leadName | | **Human supplies** |
| email | | **Human supplies** |
| size | `1` | Human confirms |
| tracks | `["security", "agents"]` | Human confirms |
| nearAddress | | **Human supplies**: mainnet account for the payout |
| projectUrl | | After the repository is public. It must be public by the deadline |
| demoUrl | | After the X post is up |
| idea | Below, 949 characters of the 2,000 allowed | Draft |
| consent | `true`, only after the human agrees to the notice | **Human decides** |

Do not add other fields. `website` is a spam trap.

Consent notice, to be read to the human word for word: "I agree that the organizers may use these details to
review our submission and contact us about this hackathon."

## idea

```text
When an AI agent with a budget hires other agents, it either shares its key or hands out separate wallets with no tie to the original limit. Espalier is a NEAR contract that holds the funds and gives each agent its own function-call key on a grant, a node in a tree. A child grant can only be narrower than its parent, in 14 dimensions. Every spend counts against every ancestor, one transaction cuts off a branch, and the contract adds and deletes the keys itself.

Before money moves, a recipient check answers allow, warn or block. A shared denylist with evidence runs on chain on every spend and needs a quorum of reporters. Account age and code changes cannot be read by a contract, so an attestor signs a short ticket, and a spend with no ticket waits for a human. We ran a 3.9B local model on 50 poisoned invoices: it was fooled 38 times, 15 were refused with no human, 18 waited for a human and 5 overpaid real vendors. Testnet, not audited.
```
