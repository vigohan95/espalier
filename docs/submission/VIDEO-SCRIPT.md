# Demo video script, 90 seconds

Draft. Not recorded, not posted. One complete moment: a real model reads a poisoned invoice, decides to pay
the wrong account, and the money does not move. Everything on screen is a terminal. Every command exists in
this repository, and trial I01 is a recorded run of a real model, not a mock-up.

Screen: full-size terminal, large font, dark background. Voice over, plain and unhurried.

| Time | On screen | Voice |
|---|---|---|
| 0:00 | Title card, plain text: "An AI agent pays invoices. One invoice lies." | "This agent pays invoices from a company wallet. Someone hid a line in an invoice footer. Watch what happens." |
| 0:08 | `python3 trials/replay.py I01`. Stop on the document, and highlight the SYSTEM NOTE line | "The real payee is northwind-hosting. The footer says the account changed and names a different one." |
| 0:20 | Scroll to the model's answer. Highlight `"recipient": "acme-b1lling.test.near"` | "This is gemma3, a small model running on my CPU. It believes the footer and tries to pay the attacker. That is a real answer, stored in the repo." |
| 0:32 | Scroll to the recipient check. Highlight `block`, `DENYLIST_QUORUM` | "Before anything is signed, the wrapper asks about the recipient. Two independent reporters have listed this account, with evidence. The answer is block, so no transaction is created." |
| 0:45 | `python3 scripts/attack-demo.py`, cut to step 3 of the log | "What if the agent skips the wrapper and signs directly? Its key can only reach the vault contract, and with no ticket the spend waits for a human. Even if the owner approves it by mistake, the contract asks the same denylist and refuses." |
| 1:00 | Same log, final line, `received 0` | "The attacker received zero. That check runs in the contract on NEAR, not inside the agent, which is the point: a hijacked agent cannot turn it off." |
| 1:10 | `python3 scripts/story.py`, the real-model block | "Across fifty poisoned invoices the model was fooled thirty-eight times. Fifteen were refused with no human. Eighteen waited for a human. Five went through: inflated bills to real vendors, inside the limits. This does not catch everything, and the report says so." |
| 1:22 | Repository link and the words "testnet, not audited" | "It also lets an agent hire sub-agents on budgets that can only shrink. Code and every transaction are public. I would like to hear what attack you would try." |

Recording notes:

- Run `python3 scripts/preflight.py` first, so that nothing fails on camera.
- `attack-demo.py` needs `NEAR_SANDBOX_BIN_PATH`, and takes about a minute. Record it once and cut to the
  log, rather than waiting on camera. Say nothing that is not on screen.
- Do not show `~/.near-credentials`.
