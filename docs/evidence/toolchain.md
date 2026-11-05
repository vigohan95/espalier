# Toolchain, as installed and run

WSL Ubuntu 24.04, root. Installed by `scripts/00-toolchain.sh`, `00b-tools-bg.sh` and `00c-sandbox-bin.sh`.

```
rustc 1.99.1 (58cf8f9c3 2026-10-13)
cargo 1.99.1 (5f94df478 2026-08-27)
cargo-near-near 0.22.0
near-cli-rs 0.30.1
neard (release 2.14.0) (build 2.14.0) (commit bc4e9e8cfb03d9e82f027883055624e18576993e) (rustc 1.95.0) (min_protocol 84) (protocol 88) (db 51)
near-sdk 5.30.0, near-workspaces 0.24.0, near-contract-standards 5.30.0
```

What was checked rather than assumed:

- near-sdk 5.30.0 sets MSRV 1.95 and targets nearcore 2.14 (protocols 87 and 88). Stable rustc 1.99.1
  was used. Wasm comes from `cargo near build non-reproducible-wasm --no-abi`, which runs
  `wasm-opt -O --enable-nontrapping-float-to-int --enable-bulk-memory` as a post step. The result
  deploys and runs on sandbox 2.14.0 and on testnet.
- near-sdk 5.30 refuses to compile on the host without `unit-testing` or similar, so a crate with
  integration tests needs `near-sdk = { features = ["unit-testing"] }` under dev-dependencies.
- The cargo-near and near-cli-rs prebuilt installers timed out here, so both were built from source
  (about 25 minutes). The near-workspaces build script streams the sandbox binary at about 60 KB/s,
  so `00c-sandbox-bin.sh` fetches the same tarball in parallel and sets `NEAR_SANDBOX_BIN_PATH`.
- `toolchain-check/` proves the assumption the whole design rests on, on a real node: a contract can
  `AddKey` a function-call key on its own account, that key can sign, the protocol enforces its
  method list, and the contract can `DeleteKey` it. Run: `scripts/hello-test.sh` (1 test, passes).
