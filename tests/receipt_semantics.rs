//! Receipt-semantics regression (2026-09-10 audit fix): cross-contract
//! receipt to an UNKNOWN account must fail ONLY the receipt — the parent
//! commits, and the callback runs with a Failed promise_result (the
//! "MPC down → refund bets" recovery path that was untestable before).
//!
//! Raw-wasm contract (wasm-encoder, no SDK):
//!   close_round():
//!     storage_write("round", "Rolling")          // entry state — must COMMIT
//!     p0 = promise_batch_create("mpc.missing.test.near")
//!     promise_batch_action_function_call_add(p0, "ping", "", gas)
//!     p1 = promise_then(p0, "casino.test.near")  // callback on self
//!     promise_batch_action_function_call_add(p1, "on_result", "", gas)
//!     promise_return(p1)
//!   on_result():
//!     r = promise_result(0, reg0)                // 0 = Failed
//!     storage_write("last_result", "0"/"1")
//!     if r == 0 { storage_write("recovery", "refunded") }

use near_mock::chain::MockChain;
use wasm_encoder::{
    CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    Instruction, Module, TypeSection, ValType,
};

const CASINO: &str = "casino.test.near";
const MPC: &str = "mpc.missing.test.near"; // NOT deployed — receipt must fail

// data-segment offsets
const ROUND: i64 = 1024; // "round"
const ROLLING: i64 = 1032; // "Rolling"
const LAST_RESULT: i64 = 1050; // "last_result"
const REFUNDED: i64 = 1070; // "refunded"
const RECOVERY: i64 = 1090; // "recovery"
const MPC_ACCT: i64 = 1110; // 24B
const PING: i64 = 1150; // "ping"
const EMPTY: i64 = 1155; // ""
const ZERO: i64 = 1200; // "0"
const ONE: i64 = 1202; // "1"
const CASINO_ACCT: i64 = 1210; // 16B
const ON_RESULT: i64 = 1240; // "on_result"
const DEPOSIT: i64 = 1300; // 16 zero bytes

// import indices (order of registration below)
const H_STORAGE_WRITE: u32 = 0;
const H_BATCH_CREATE: u32 = 1;
const H_FC_ADD: u32 = 2;
const H_THEN: u32 = 3;
const H_RETURN: u32 = 4;
const H_RESULTS_COUNT: u32 = 5;
const H_PROMISE_RESULT: u32 = 6;

fn gas() -> i64 {
    30_000_000_000_000
}

fn close_round_fn() -> Function {
    // locals: 0 = p0, 1 = p1 (both i64)
    let mut f = Function::new(vec![(2, ValType::I64)]);
    // round = "Rolling"  (commit-probe written by the ENTRY)
    f.instruction(&Instruction::I64Const(5));
    f.instruction(&Instruction::I64Const(ROUND));
    f.instruction(&Instruction::I64Const(7));
    f.instruction(&Instruction::I64Const(ROLLING));
    f.instruction(&Instruction::I64Const(-1)); // rid: none (u64::MAX)
    f.instruction(&Instruction::Call(H_STORAGE_WRITE));
    f.instruction(&Instruction::Drop);
    // p0 = promise_batch_create(24, MPC_ACCT)
    f.instruction(&Instruction::I64Const(24));
    f.instruction(&Instruction::I64Const(MPC_ACCT));
    f.instruction(&Instruction::Call(H_BATCH_CREATE));
    f.instruction(&Instruction::LocalSet(0));
    // p0.ping("")  [deposit ptr = 16 zero bytes, gas]
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(4));
    f.instruction(&Instruction::I64Const(PING));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64Const(EMPTY));
    f.instruction(&Instruction::I64Const(DEPOSIT));
    f.instruction(&Instruction::I64Const(gas()));
    f.instruction(&Instruction::Call(H_FC_ADD));
    // p1 = promise_then(p0, casino, "on_result", "") — the mock's composed
    // then: creates the callback batch AND its function call in one host
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(16));
    f.instruction(&Instruction::I64Const(CASINO_ACCT));
    f.instruction(&Instruction::I64Const(9));
    f.instruction(&Instruction::I64Const(ON_RESULT));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64Const(EMPTY));
    f.instruction(&Instruction::I64Const(0)); // unused arg slot
    f.instruction(&Instruction::I64Const(gas()));
    f.instruction(&Instruction::Call(H_THEN));
    f.instruction(&Instruction::LocalSet(1));
    // promise_return(p1)
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::Call(H_RETURN));
    f.instruction(&Instruction::End);
    f
}

fn on_result_fn() -> Function {
    // locals: 0 = r (i64)
    let mut f = Function::new(vec![(1, ValType::I64)]);
    // r = promise_result(0, reg 0)
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::Call(H_PROMISE_RESULT));
    f.instruction(&Instruction::LocalSet(0));
    // last_result = "0" if r==0 else "1"  → ptr = ZERO + r*2
    f.instruction(&Instruction::I64Const(11));
    f.instruction(&Instruction::I64Const(LAST_RESULT));
    f.instruction(&Instruction::I64Const(1));
    // (value ptr pushed next, then rid)
    // addr = (ZERO as i32) + (r == 0 ? 0 : 2)
    f.instruction(&Instruction::I64Const(ZERO));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Const(2));
    f.instruction(&Instruction::I64Mul);
    f.instruction(&Instruction::I64Add);
    f.instruction(&Instruction::I32WrapI64);
    f.instruction(&Instruction::I64ExtendI32U);
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Call(H_STORAGE_WRITE));
    f.instruction(&Instruction::Drop);
    // if r == 0: recovery = "refunded"
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I64Eqz);
    f.instruction(&Instruction::If(wasm_encoder::BlockType::Empty));
    f.instruction(&Instruction::I64Const(8));
    f.instruction(&Instruction::I64Const(RECOVERY));
    f.instruction(&Instruction::I64Const(8));
    f.instruction(&Instruction::I64Const(REFUNDED));
    f.instruction(&Instruction::I64Const(-1));
    f.instruction(&Instruction::Call(H_STORAGE_WRITE));
    f.instruction(&Instruction::Drop);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f
}

fn build() -> Vec<u8> {
    let mut types = TypeSection::new();
    types.ty().function(vec![], vec![]); // t0: our methods ()->()
    types.ty().function(vec![ValType::I64; 5], vec![ValType::I64]); // t1: storage_write
    types
        .ty()
        .function(vec![ValType::I64, ValType::I64], vec![ValType::I64]); // t2: batch_create
    types.ty().function(vec![ValType::I64; 7], vec![]); // t3: fc_add
    types.ty().function(vec![ValType::I64; 9], vec![ValType::I64]); // t4: then (composed)
    types.ty().function(vec![ValType::I64], vec![]); // t5: return
    types.ty().function(vec![], vec![ValType::I64]); // t6: results_count
    types
        .ty()
        .function(vec![ValType::I64, ValType::I64], vec![ValType::I64]); // t7: promise_result

    let mut imports = ImportSection::new();
    imports.import(
        "env",
        "storage_write",
        wasm_encoder::EntityType::Function(1),
    );
    imports.import(
        "env",
        "promise_batch_create",
        wasm_encoder::EntityType::Function(2),
    );
    imports.import(
        "env",
        "promise_batch_action_function_call",
        wasm_encoder::EntityType::Function(3),
    );
    imports.import("env", "promise_then", wasm_encoder::EntityType::Function(4));
    imports.import(
        "env",
        "promise_return",
        wasm_encoder::EntityType::Function(5),
    );
    imports.import(
        "env",
        "promise_results_count",
        wasm_encoder::EntityType::Function(6),
    );
    imports.import(
        "env",
        "promise_result",
        wasm_encoder::EntityType::Function(7),
    );

    // defined funcs: indices 7 (close_round), 8 (on_result) after 7 imports
    let mut funcs = FunctionSection::new();
    funcs.function(0);
    funcs.function(0);

    let mut exports = ExportSection::new();
    exports.export("close_round", ExportKind::Func, 7);
    exports.export("on_result", ExportKind::Func, 8);
    exports.export("memory", ExportKind::Memory, 0);
    let mut memory = wasm_encoder::MemorySection::new();
    memory.memory(wasm_encoder::MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut data = wasm_encoder::DataSection::new();
    let mut blob = vec![0u8; 1400];
    fn put(b: &mut [u8], off: usize, s: &[u8]) {
        b[off..off + s.len()].copy_from_slice(s);
    }
    put(&mut blob, ROUND as usize, b"round");
    put(&mut blob, ROLLING as usize, b"Rolling");
    put(&mut blob, LAST_RESULT as usize, b"last_result");
    put(&mut blob, REFUNDED as usize, b"refunded");
    put(&mut blob, RECOVERY as usize, b"recovery");
    put(&mut blob, MPC_ACCT as usize, MPC.as_bytes());
    put(&mut blob, PING as usize, b"ping");
    put(&mut blob, ZERO as usize, b"0");
    put(&mut blob, ONE as usize, b"1");
    put(&mut blob, CASINO_ACCT as usize, CASINO.as_bytes());
    put(&mut blob, ON_RESULT as usize, b"on_result");
    data.active(0, &ConstExpr::extended([Instruction::I32Const(0)]), blob);

    let mut code = CodeSection::new();
    code.function(&close_round_fn());
    code.function(&on_result_fn());

    let mut module = Module::new();
    module.section(&types).section(&imports).section(&funcs);
    module.section(&memory).section(&exports).section(&code);
    module.section(&data);
    module.finish()
}

#[test]
fn unknown_account_receipt_fails_receipt_not_parent() {
    let wasm = build();
    let chain = MockChain::builder()
        .contract_bytes(CASINO, wasm)
        .signer("player.test.near")
        .now(1_788_000_000)
        .build()
        .expect("build chain");

    let out = chain
        .call(CASINO, "close_round")
        .args("{}")
        .fire()
        .expect("fire");

    // 1. the PARENT commits — the casino case's "Rolling" state survives
    let round = chain
        .storage_get(CASINO, b"round")
        .map(|v| String::from_utf8_lossy(&v).into_owned());
    assert_eq!(round.as_deref(), Some("Rolling"), "entry state must COMMIT");

    // 2. the callback RAN and saw Failed (promise_result == 0)
    let last = chain
        .storage_get(CASINO, b"last_result")
        .map(|v| String::from_utf8_lossy(&v).into_owned());
    assert_eq!(
        last.as_deref(),
        Some("0"),
        "callback must run and read Failed"
    );

    // 3. the recovery path executed — THE thing that was untestable before
    let recovery = chain
        .storage_get(CASINO, b"recovery")
        .map(|v| String::from_utf8_lossy(&v).into_owned());
    assert_eq!(
        recovery.as_deref(),
        Some("refunded"),
        "recovery handler must run on Failed receipt"
    );

    // 4. the failure is recorded (visible, not silent — the 2026-09-02 concern)
    assert!(
        out.receipt_failures
            .iter()
            .any(|f| f.contains("AccountDoesNotExist")),
        "receipt_failures must name the missing account: {:?}",
        out.receipt_failures
    );

    // 5. tx status: final callback SUCCEEDED (it wrote markers) ⇒ tx ok
    assert!(
        out.ok,
        "tx follows the final receipt; the callback succeeded: {:?}",
        out.error
    );
}

#[test]
fn deferred_receipts_freeze_the_incident_then_settle_recovers() {
    let wasm = build();
    let chain = MockChain::builder()
        .contract_bytes(CASINO, wasm)
        .signer("player.test.near")
        .now(1_788_000_000)
        .build()
        .expect("build chain");

    // ── ACT 1: the incident. Entry commits; receipts hang in the queue. ──
    let out = chain
        .call(CASINO, "close_round")
        .args("{}")
        .fire_deferred()
        .expect("fire_deferred");
    assert!(out.ok, "entry receipt committed");

    // the ON-CHAIN stuck state: round = Rolling, MPC silent, NO recovery
    assert_eq!(
        chain.storage_get(CASINO, b"round").as_deref(),
        Some(b"Rolling" as &[u8]),
        "round stuck in Rolling"
    );
    assert!(
        chain.storage_get(CASINO, b"recovery").is_none(),
        "recovery must NOT have run — MPC receipt undelivered"
    );
    assert!(
        chain.storage_get(CASINO, b"last_result").is_none(),
        "callback must NOT have run yet"
    );
    assert_eq!(
        chain.pending_receipts(),
        2,
        "two receipts queued: MPC request + on_result callback"
    );

    // time passes on a stuck round — the incident, frozen and inspectable
    chain.advance(600);

    // ── ACT 2: the delivery. Receipts settle in causal order. ──
    let report = chain.settle().expect("settle");
    assert_eq!(report.delivered, 2, "both receipts delivered");
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.contains("AccountDoesNotExist")),
        "MPC receipt failed: {:?}",
        report.failures
    );
    // recovery ran AFTER the failure — the path that was untestable
    assert_eq!(
        chain.storage_get(CASINO, b"recovery").as_deref(),
        Some(b"refunded" as &[u8]),
        "recovery handler ran at settle time"
    );
    assert_eq!(
        chain.storage_get(CASINO, b"last_result").as_deref(),
        Some(b"0" as &[u8]),
        "callback read Failed (ABI 0)"
    );
    // the queue drained
    assert_eq!(chain.pending_receipts(), 0);
}

#[test]
fn deferred_receipts_survive_intervening_transactions() {
    // chain property: queued receipts execute against CURRENT state —
    // transactions fired between defer and settle land first.
    let wasm = build();
    let chain = MockChain::builder()
        .contract_bytes(CASINO, wasm)
        .signer("player.test.near")
        .now(1_788_000_000)
        .build()
        .expect("build chain");

    chain
        .call(CASINO, "close_round")
        .args("{}")
        .fire_deferred()
        .expect("fire_deferred");
    assert_eq!(chain.pending_receipts(), 2);

    // an unrelated tx executes while the MPC receipt hangs
    let out = chain.call(CASINO, "close_round").args("{}").fire().expect("fire");
    assert!(out.ok);

    // the queue survived the intervening tx and settles against new state
    let report = chain.settle().expect("settle");
    assert_eq!(report.delivered, 2);
    assert_eq!(
        chain.storage_get(CASINO, b"recovery").as_deref(),
        Some(b"refunded" as &[u8])
    );
}
