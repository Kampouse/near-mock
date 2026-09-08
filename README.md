# near-mock

**A NEAR blockchain with the network removed.** Execute real smart contracts —
unmodified wasm — in-process, deterministically, in milliseconds. No node, no
Docker, no RPC, no fees.

```bash
cargo install near-mock   # or: git clone + cargo build --release
```

## Why

| | near-sandbox | near-mock |
|---|---|---|
| Engine | real node in Docker | wasmtime in-process |
| Startup | seconds | instant |
| Deterministic replay | no | yes (pin seed/time/epoch) |
| Real crypto precompiles | yes | yes (same crates) |
| Gas metering | protocol-accurate | PV155 fee schedule |
| CI-friendly | flaky, slow | trivially hermetic |

Contracts only ever see host calls, state, and gas. near-mock provides all
three faithfully, so real contracts (vaults, DEXes, oracles, Burrow) run
unmodified and behave the same — just instantly.

## Quickstart

```bash
# 1. Snapshot real mainnet state (any contract; needs an RPC that serves it)
near-mock snapshot pyth-oracle.near pyth.bin --rpc https://archival-rpc.mainnet.near.org

# 2. Call it locally
near-mock pyth.bin --view some_method '{}'

# 3. Or script a whole cross-contract flow
near-mock scenario my_flow.json
```

## What's real

- **Host surface (~140 fns)**: storage, registers, promises (batch actions:
  transfer, function_call, stake, add_key, delete_key, delete_account),
  validators, epochs — production semantics, Deprecated APIs trap loudly.
- **Crypto**: ecrecover (k256, low-s policy), p256_verify, alt_bn128
  g1_sum/multiexp/pairing_check (zeropool-bn — same crate as nearcore),
  keccak512, BLS12-381. Byte-identical encodings.
- **Receipts execute once** (memoized per node), state changes drain with
  per-receipt atomic revert — on-chain semantics without the chain.
- **Gas**: protocol V155 fee table from near-parameters 0.37.3, fuel-metered
  by wasmtime, `--trace` shows per-host costs.

## Determinism controls

```bash
NEAR_MOCK_SEED=abc123          # pin random_seed
NEAR_MOCK_NOW=1700000000       # pin block timestamp (--advance to time-travel)
NEAR_MOCK_EPOCH=500            # pin epoch_height
NEAR_MOCK_VALIDATORS='{"alice.pool.near": "1000000"}'  # pin validator set
NEAR_MOCK_STATE=/tmp/meal.bin  # isolated state file
```

Same inputs → same outputs, every time. Perfect for regression tests.

## Introspection

- `--trace` — host-call timeline + per-host gas
- `--json` — machine-readable result line
- `symbolicate` — map trap indices back to function names
- `exports` / `imports` — contract surface inventory
- `state dump <file.bin> [prefix]` — read raw trie contents

## Known limits (by design)

No consensus, sharding, finality delays, validator economics, or
function-call-access key *permission enforcement* beyond key-existence checks
(ED25519 enforced; permission lists accepted but not policed). random_seed is
seeded, not beacon-derived.

## Status

Battle-tested against Burrow margin flows (cross-contract, callbacks, batch
actions) with live-mainnet snapshots. 52-check hermetic verify suite.
