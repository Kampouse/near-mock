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
| Gas metering | protocol-accurate | protocol-accurate (finite-wasm instrumentation, PV155) |
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

# 4. Make your project agent-aware
near-mock skill    # installs .agents/skills/near-mock/ (SKILL.md + scenario example)
```

## The AI-agent skill

`near-mock skill` writes `.agents/skills/near-mock/` into the current
project — coding agents (Zed, etc.) working there automatically learn the
runner's patterns: the four execution modes, determinism controls, the
scenario-runner step fields, live-snapshot forensics, state surgery, and
the chain-parity verification technique. `--stdout` prints it instead;
`--force` overwrites. Every scaffolded `near-compile init` project gets a
sibling near-compile skill; this one covers the runner side for contracts
of any origin (Rust near-sdk included).

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

```

Big contracts are pulled with cursor pagination (consistent to one pinned
block, shown in the summary). If your RPC caps contract-state reads and the
snapshot aborts with `TOO_LARGE_CONTRACT_STATE`, point it at a full-cap
provider: `--rpc https://rpc.intea.rs` (wrap.near-sized tries pull fine).
bash
NEAR_MOCK_SEED=abc123          # pin random_seed
NEAR_MOCK_NOW=1700000000       # pin block timestamp (--advance to time-travel)
NEAR_MOCK_EPOCH=500            # pin epoch_height
NEAR_MOCK_VALIDATORS='{"alice.pool.near": "1000000"}'  # pin validator set
NEAR_MOCK_STATE=/tmp/meal.bin  # isolated state file
```

Same inputs → same outputs, every time. Perfect for regression tests.

## Library use (Rust)

`MockChain` gives you the whole engine in-process — for CI tests that need no
CLI, no state files, no process spawning:

```rust
use near_mock::chain::MockChain;

let chain = MockChain::builder()
    .contract("guestbook.test.near", "guestbook.wasm")?
    .signer("alice.test.near")
    .now(1_788_000_000)                    // deterministic clock
    .build()?;

let tx = chain.call("guestbook.test.near", "sign")
    .args(r#"{"message":"hi"}"#).attach(1_000_000).fire()?;
assert!(tx.ok);
assert!(tx.gas_burned > 0);                // PV155-metered

let n = chain.view("guestbook.test.near", "get_signature_count").fire()?;
assert_eq!(n.return_string().as_deref(), Some("1"));

chain.advance(3_600)?;                     // time-travel
chain.save()?;                             // persist state file
```

Transaction semantics match mainnet: entry trap or failed receipt chain
→ full rollback (deposit refunds); fire-and-forget receipts commit
independently; `fail_receipt(n)` forces receipt failures for rollback tests.
One `MockChain` per thread (the engine is thread-local). See
`tests/chain_api.rs` for working examples.

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
actions) with live-mainnet snapshots. 68-check hermetic verify suite.

## 0.2.0 — mainnet-parity gas engine

Gas is no longer an approximation. Every contract is instrumented with the same
finite-wasm pass mainnet uses (`prepare_v3` port), metering PV155 costs into an
exported `remaining_gas` global — wasmtime fuel is gone.

- **Instruction gas**: identical instrumentation + cost model (regular_op_cost,
  control flow free, bulk ops = base + unit × runtime length)
- **Host costs**: full PV155 composites — per-call base, read/write_memory,
  register costs, utf8 decoding on logs, storage + trie, crypto precompiles
- **Action fees**: receipt creation + function-call base/byte, entry and every
  promise sub-receipt
- **Stack limit**: 262,144-frame instrumented budget (mainnet's model)
- **Limits**: memory capped at 2048 pages, registers 100 MiB/1 GiB (mainnet values)
- **NaN canonicalization** on (mainnet setting); OOG traps with mainnet's message

Also: `MockChain` outcomes now expose per-tx logs, normalized panic classes,
and promise-receipt failures; `args_bytes()` for binary-arg contracts; a
`stream_replay` example (live mainnet differential replayer) and `vm_limits`
parity tests. Fixed: promise-outcome memo leaked across transactions (infinite
drain loop in long-running processes).

Known calibration gap: trie-node charges use a flat walk model (mainnet's scale
with real trie/proof size) — measured ~0.6x on 10+ GB contracts, ~0.9x+ on
small ones.

## Fork-mode — real chain state, locally

```bash
near-mock fork <account> <method> [args-json] [--url RPC] [--block H] [--signer S] [--deposit YOCTO] [--view]
```

Or in code:

```rust
let chain = MockChain::builder()
    .fork("https://rpc.mainnet.fastnear.com", Some(123_456_789))
    .build()?;
let out = chain.view("omft.near", "acl_is_super_admin")
    .args(r#"{"account_id":"omft.near"}"#)
    .fire()?;
```

Contract code (`view_code`) and storage (`view_state`) are paged in lazily
at a pinned block; writes land locally (tombstones keep deletions from
resurrecting). Validated byte-identical to RPC's own view execution
(`tests/fork.rs --ignored`).

`--block` accepts a height (pinned, reproducible forever), `final`
(always-fresh `finality: "final"` on every fetch — state may drift
mid-session; opt-in liveness over determinism), or is omitted (latest,
resolved once and pinned).

**No state-size limit**: unpaginated `view_state` refuses large contracts
(`TOO_LARGE_CONTRACT_STATE`), but the paginated path (`limit` +
`after_key_base64`) has no such check — fork-mode pages through it.
Validated byte-identical on the biggest contracts on mainnet (wrap.near
158MB, token.sweat, intents.near 11.7GB).

## replay — "why did my tx fail?" as a one-liner



Fetches the real transaction, forks mainnet state at the block before it
executed, replays the entry receipt with the real predecessor/args/deposit,
and diffs status/logs/gas against mainnet's recorded outcome. 
adds the full host-call timeline — internals mainnet can never show.

## 0.4.0 — chain-faithful receipt semantics

Receipts are independent atomic units (like the chain):
- a cross-contract receipt to an UNKNOWN account fails ONLY the receipt —
  the parent commits, callbacks receive a Failed promise_result
  (recovery paths — "MPC down, refund bets" — are testable now)
- the tx status follows the FINAL receipt of the returned chain; earlier
  receipts' state stays committed (no whole-tx rollback)
- promise_result ABI fix: Failed = 0 (was 2, which near-sdk parses as
  NotReady — recovery handlers never fired)
- bare wasm traps classify into nearcore's WasmTrap taxonomy (message-less
  release builds still compare by failure class)

## 0.5.0 — deferred receipts: the async chain model, on demand

```rust
let out = chain.call(casino, "close_round").fire_deferred()?;  // entry commits,
// receipts QUEUE — the stuck state is observable for as long as you like
chain.advance(600);                                            // let it hang
let report = chain.settle()?;                                  // deliver in causal
// order — callbacks run, recovery paths execute, failures propagate
```

fire() still auto-drains (sync, backwards compatible). fire_deferred() is
the chain's actual model: receipts are independent units delivered in
later blocks. Incident forensics — "round stuck in Rolling while MPC is
silent" — is now a deterministic two-act script you can freeze, inspect,
time-travel, and resolve. Queued receipts survive intervening
transactions and settle against current state, exactly like the chain.
