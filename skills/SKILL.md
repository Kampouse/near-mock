---
name: near-mock
description: >-
    Run and test NEAR smart contracts locally with full host semantics —
    no node, no network, deterministic. Use when a task involves testing,
    debugging, or forensically analyzing NEAR contracts of ANY origin
    (Rust near-sdk, near-contract-standard, or lisp-rlm output): local
    execution, live-state snapshots, state surgery, scenario flows, or
    gas/host-call analysis.
---

# near-mock — local NEAR contract runner (no node)

Real wasm, real host surface (~140 fns incl. protocol 69/72), deterministic
clock/entropy, receipt-atomic state — executed in-process in milliseconds.

```bash
cargo install near-mock    # 0.1.8+
```

## The four modes

```bash
# 1. single call (fresh state per default file)
near-mock out.wasm method '{"json":"args"}' [--view]

# 2. cross: multi-contract world in one state file
near-mock cross state.bin acct=/path/a.wasm,acct2=/path/b.wasm acct2 method '{}'

# 3. scenario: scripted multi-step flows (see example-scenario.json)
near-mock scenario flow.json

# 4. snapshot: pull a LIVE contract + state for local forensics
near-mock snapshot some-contract.testnet state.bin --rpc https://rpc.testnet.near.org
near-mock cross state.bin some-contract.testnet=some-contract.testnet.wasm \
  some-contract.testnet some_view '{}' --view
```

## Environment controls (determinism + identity)

| Var / flag | Effect |
|---|---|
| `NEAR_MOCK_SIGNER` | signer + predecessor for the call |
| `NEAR_MOCK_ATTACH` / `--attach` / `--deposit` | attached deposit (yocto) — flags work in ALL modes incl. `cross` (≥0.1.7) |
| `NEAR_MOCK_NOW` / `--now` | pin block timestamp (unix secs) |
| `--advance <secs>` | time-travel (scenario steps accumulate it) |
| `NEAR_MOCK_SEED` | pin random_seed |
| `NEAR_MOCK_STATE` / `--state` | state file (default /tmp/near-mock-state.bin) |
| `--view` | read-only: writes refused (ProhibitedInView), no persist |
| `--dry-run` | execute + report, don't persist |
| `--staking` | enforce 1e20 yocto/byte storage staking |
| `--json` | machine-readable outcome (return value, gas, storage diff, events) — ALL modes incl. `cross` (≥0.1.8) |
| `--trace` | host-call timeline + per-host gas (incl. error counts since 0.1.2) |

## Exit codes (CI-safe since 0.1.1)

`0` success/view/dry-run · `1` contract trap, out-of-gas, failed receipt
chain. `near-mock x.wasm m || fail` works in scripts; `--json`'s
`"outcome"` field agrees with `$?`.

## Scenario runner

Steps support `method`, `contract`, `args`, `view`, `as` (signer),
`predecessor`, `now`, `advance` (monotonic), `gas` (TGas cap),
`attach` (yocto), `expect` (substring or `"trap"` — traps roll back),
`fail_receipt` (forced receipt failure), `snapshot`/`restore`/
`expect_same_storage_as`. The runner expects `contract.wasm` beside the
JSON unless a `manifest` maps accounts. See `example-scenario.json` in this
skill directory — it is a complete runnable template.

## Live-contract forensics (the snapshot workflow)

1. `near-mock snapshot <acct> <state.bin> --rpc <testnet-rpc>` — wasm +
   storage at one block (the `.wasm` lands in the cwd).
2. `exports` / `imports` — inventory the surface; imports reveal the SDK
   generation (protocol 69/72 hosts = current near-contract-standard).
3. Run views via `cross`. Borsh STATE blobs: `state dump` + base64 decode
   + read the printable strings, or call the contract's own JSON views.
4. Mutate freely — it's a local copy. Traps roll back atomically.
5. Chain-parity proof: fetch deployed bytes via RPC `view_code`, replay
   under the mock — byte-identical answers prove runner fidelity (this is
   how the value_return last-write-wins bug was caught).

## State tooling

```bash
near-mock state dump <state.bin> [acct-prefix]   # stdout = pure JSON
near-mock state import <state.bin> <dump.json>   # canonical AND legacy shapes
near-mock reset                                   # clear default state file
```

Storage is namespaced per account (`acct\x01key`); `--staking` tracks
locked balances; storage-staking gates are enforced by contracts
themselves via `depositGte` (ONE u128 as (lo64, hi64) — e.g. ~0.01 NEAR
= `depositGte(0, 542)`; result is boolean-tagged).

## Coverage facts

- near-sdk 4.x/5.x, near-contract-standard (old + current import sets):
  all instantiate. `transfer_to_gas_key`, `add_gas_key_*`,
  `deploy/use_global_contract*`, `current_contract_code`, `chain_id` are
  bound with documented mock approximations (loud, never silent).
- Crypto is real: BIP-340 schnorr (stitched-lib parity verified against
  reference vectors), ed25519, ecrecover, alt_bn128, BLS12-381, sha2/3.
- `value_return` is last-write-wins like nearcore (≥0.1.5) — multi-return
  contracts behave identically on mock and chain.

## Install the skill into a project

```bash
near-mock skill              # writes .agents/skills/near-mock/ here
near-mock skill --stdout     # print instead of installing
```
