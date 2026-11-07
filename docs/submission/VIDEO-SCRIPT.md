# Demo video script, 116 seconds

Built, not posted. `python docs/submission/video/build.py` draws the film from the recorded testnet run.
The text and timing live in [video/storyboard.json](video/storyboard.json), and `video/out/timing.json`
holds each caption as it was rendered, with its numbers filled in.

One idea: **auctions grow a budget tree, and one revoke cuts a branch.** The earlier script, about a poisoned
invoice, is gone: that story is the most common one in the security track, and the injection numbers are
still in the README.

It works with the sound off. Every sentence is on screen, and timing follows reading speed, not a voice.
The silent file is the main one. `--voice` also writes `espalier-demo-voiced.mp4`, the same frames with a
synthetic voice. Whether any voice goes out is the human's decision.

| Scene | On screen | Caption |
|---|---|---|
| Title | Plain text | Agents hire agents, who hire agents. Who keeps the third one inside the budget? |
| Root | One node, and a counter of what the root has been charged | An owner gives a general contractor a budget |
| Auction | Task 1's four bids as the hashes on chain, then as they opened | Sealed bids. dee never reveals and loses its bond. ada bid 200 and gets 230, the next bid |
| Growth | The tree grows one node per award, with each price on its edge | The prize is a grant, not a payment. Nothing has left the vault. eve is refused `OVERCOMMITTED`. 13 auctions, 12 grants |
| Hijack | hal turns amber, and a crossed-out line runs to an outsider | "Suppose hal is hijacked." The vault refuses the payment and the wider grant |
| Revoke | hal and the three agents below it turn red in the same instant, with the transaction hash | One transaction. Keys deleted. Tasks void, bonds back |
| Money | Meters fill up the lineage as agents draw, and the counter reaches 430 | fay draws 30, charged to every level above. ada is refused `CAP`. 330 paid, 100 back to the vault |
| Page | A slow pan down `site/index.html`, rendered from the run's snapshot | Read-only, no wallet, no key |
| Verify | `docs/evidence/verify-market.log` | One command, no keys |
| End | Test numbers, repository link, status | The agents are scripted. The contracts and all 208 transactions are real |

What keeps it true:

- Every number in a caption comes from `docs/evidence/`. None is typed into the storyboard.
- A caption can name `proof`, text that must be a passed check in `swarm-sim-testnet.json`, or the build stops.
- The meters replay the run's draws, and the build stops unless they end on what the vault reported.
- "Suppose" is deliberate: the hijack in this run is scripted. The end card says the agents are scripted.

Before the repository goes public, check that the end card's link works.
