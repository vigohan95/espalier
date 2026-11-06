# Market probe

A throwaway contract and script that test the assumptions behind [docs/SWARM-RULES.md](../../SWARM-RULES.md)
against the committed, unchanged vault wasm on a real local node. It is not the market, and it goes away
once the market's own scenarios cover the same checks.

```sh
(cd docs/evidence/market-probe && cargo near build non-reproducible-wasm --no-abi)
NEAR_SANDBOX_BIN_PATH=/path/to/near-sandbox python3 docs/evidence/market-probe/probe.py /path/to/market_probe.wasm
```

You see one `PASS` line per assumption and `34/34 checks passed`. The run of 2026-11-06 is in
[../market-probe.log](../market-probe.log) and [../market-probe.json](../market-probe.json).
The folder is its own cargo workspace, so the repository's build does not see it.
