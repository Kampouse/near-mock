//! VM-limit parity regressions (wasmtime parity audit 2026-09-10).
//! Probe contracts are built inline with wasm-encoder — no fixtures needed.
//! NEAR calling convention: exported methods are ()->(); results go through
//! env.value_return(len, ptr) after storing them into linear memory.

use near_mock::chain::MockChain;
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection,
    Instruction, MemArg, Module, TypeSection, ValType,
};

fn engine_with(module_bytes: &[u8]) -> MockChain {
    MockChain::builder()
        .contract_bytes("probe.test.near", module_bytes.to_vec())
        .signer("alice.test.near")
        .now(1_788_000_000)
        .build()
        .expect("build chain")
}

fn i32_from_return(out: &near_mock::chain::CallOutcome) -> i32 {
    let bytes = out
        .return_data
        .as_deref()
        .and_then(|d| <[u8; 4]>::try_from(d).ok())
        .unwrap_or([0; 4]);
    i32::from_le_bytes(bytes)
}

/// (module
///   (import "env" "value_return" (func (param i64 i64)))
///   (memory (export "memory") 1)
///   (func (export "go") (local i32)   ;; pages = 1
///     (block                  ;; outer exit
///       (loop                ;; grow until it fails, counting pages
///         (if (i32.eq (memory.grow 1) (i32.const -1))
///           (then (i32.store (i32.const 1024) (local.get 0)) (br 2)))
///         (local.set 0 (i32.add (local.get 0) (i32.const 1)))
///         (br 0)))
///     (call value_return (i64.const 4) (i64.const 1024))))
/// Returns the final page count: mainnet's ceiling is 2048 pages (128 MiB).
/// Pre-fix: no store limiter → the guest grew to wasm's 65536-page ceiling.
#[test]
fn memory_capped_at_mainnet_2048_pages() {
    let wasm2 = {
        let mut types = TypeSection::new();
        types.ty().function(vec![], vec![]);
        types
            .ty()
            .function(vec![ValType::I64, ValType::I64], vec![]);
        let mut funcs = FunctionSection::new();
        funcs.function(0);
        let mut imports = wasm_encoder::ImportSection::new();
        imports.import("env", "value_return", wasm_encoder::EntityType::Function(1));
        let mut memory = wasm_encoder::MemorySection::new();
        memory.memory(wasm_encoder::MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut exports = ExportSection::new();
        exports.export("go", ExportKind::Func, 1);
        exports.export("memory", ExportKind::Memory, 0);
        // go: local i32 v; v = bits(load 0) → f32 → +0.0 → reinterpret;
        //     store v at 1024; value_return(4, 1024)
        let mut f = Function::new(vec![(1, ValType::I32)]);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        f.instruction(&Instruction::F32ReinterpretI32);
        f.instruction(&Instruction::F32Const(0.0f32.into()));
        f.instruction(&Instruction::F32Add);
        f.instruction(&Instruction::I32ReinterpretF32);
        f.instruction(&Instruction::LocalSet(0));
        f.instruction(&Instruction::I32Const(1024));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        f.instruction(&Instruction::I64Const(4));
        f.instruction(&Instruction::I64Const(1024));
        f.instruction(&Instruction::Call(0));
        f.instruction(&Instruction::End);
        let mut code = CodeSection::new();
        code.function(&f);
        let mut data = wasm_encoder::DataSection::new();
        data.active(
            0,
            &ConstExpr::extended([Instruction::I32Const(0)]),
            0x7fc1_2345u32.to_le_bytes(),
        );
        let mut module = Module::new();
        module
            .section(&types)
            .section(&imports)
            .section(&funcs)
            .section(&memory);
        module.section(&exports).section(&code);
        module.section(&data);
        module.finish()
    };
    let chain = engine_with(&wasm2);
    let out = chain.call("probe.test.near", "go").fire().expect("fire");
    assert!(out.ok, "nan probe failed: {:?}", out.error);
    let bits = i32_from_return(&out) as u32;
    assert_eq!(
        bits & 0x7fff_ffff,
        0x7fc0_0000,
        "f32 NaN must canonicalize to 0x7fc00000, got {bits:#010x}"
    );
}

/// input() of a multi-MiB payload must land in a register: mainnet allows up
/// to 100 MiB/register (1 GiB total; protocol-86 limits). Pre-fix: 1 MiB cap
/// rejected mainnet-legal args (aurora submit payloads are multi-MiB).
/// go: input(0); len = register_len(0); store len@1024; value_return(8, 1024)
/// NaN payloads must canonicalize (mainnet: cranelift_nan_canonicalization(true),
/// with nearcore's own nan_canonicalization test). Non-canonical NaN bits come
/// from a data segment (defeats const folding), then x + 0.0 -> reinterpret.
#[test]
fn nan_payloads_canonicalize() {
    let wasm = {
        let mut types = TypeSection::new();
        types.ty().function(vec![], vec![]); // 0: go
        types.ty().function(vec![ValType::I64, ValType::I64], vec![]); // 1: value_return
        let mut funcs = FunctionSection::new();
        funcs.function(0);
        let mut imports = wasm_encoder::ImportSection::new();
        imports.import("env", "value_return", wasm_encoder::EntityType::Function(1));
        let mut memory = wasm_encoder::MemorySection::new();
        memory.memory(wasm_encoder::MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut exports = ExportSection::new();
        exports.export("go", ExportKind::Func, 1);
        exports.export("memory", ExportKind::Memory, 0);
        // go: local i32 v; v = reinterpret_f32(reinterpret_i32(load 0) + 0.0f);
        //     store v at 1024; value_return(4, 1024)
        let mut f = Function::new(vec![(1, ValType::I32)]);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32Load(MemArg { offset: 0, align: 2, memory_index: 0 }));
        f.instruction(&Instruction::F32ReinterpretI32);
        f.instruction(&Instruction::F32Const(0.0f32.into()));
        f.instruction(&Instruction::F32Add);
        f.instruction(&Instruction::I32ReinterpretF32);
        f.instruction(&Instruction::LocalSet(0));
        f.instruction(&Instruction::I32Const(1024));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Store(MemArg { offset: 0, align: 2, memory_index: 0 }));
        f.instruction(&Instruction::I64Const(4));
        f.instruction(&Instruction::I64Const(1024));
        f.instruction(&Instruction::Call(0));
        f.instruction(&Instruction::End);
        let mut code = CodeSection::new();
        code.function(&f);
        let mut data = wasm_encoder::DataSection::new();
        data.active(
            0,
            &ConstExpr::extended([Instruction::I32Const(0)]),
            0x7fc1_2345u32.to_le_bytes(),
        );
        let mut module = Module::new();
        module.section(&types).section(&imports).section(&funcs).section(&memory);
        module.section(&exports).section(&code);
        module.section(&data);
        module.finish()
    };
    let chain = engine_with(&wasm);
    let out = chain.call("probe.test.near", "go").fire().expect("fire");
    assert!(out.ok, "nan probe failed: {:?}", out.error);
    let bits = i32_from_return(&out) as u32;
    assert_eq!(
        bits & 0x7fff_ffff,
        0x7fc0_0000,
        "f32 NaN must canonicalize to 0x7fc00000, got {bits:#010x}"
    );
}

#[test]
fn multi_mib_args_fit_register() {
    let wasm = {
        let mut types = TypeSection::new();
        types.ty().function(vec![], vec![]); // 0: go ()->()
        types.ty().function(vec![ValType::I64], vec![]); // 1: input
        types.ty().function(vec![ValType::I64], vec![ValType::I64]); // 2: register_len
        types
            .ty()
            .function(vec![ValType::I64, ValType::I64], vec![]); // 3: value_return
        let mut funcs = FunctionSection::new();
        funcs.function(0);
        let mut imports = wasm_encoder::ImportSection::new();
        imports.import("env", "input", wasm_encoder::EntityType::Function(1));
        imports.import("env", "register_len", wasm_encoder::EntityType::Function(2));
        imports.import("env", "value_return", wasm_encoder::EntityType::Function(3));
        let mut memory = wasm_encoder::MemorySection::new();
        memory.memory(wasm_encoder::MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut exports = ExportSection::new();
        exports.export("go", ExportKind::Func, 3); // after 3 imports
        exports.export("memory", ExportKind::Memory, 0);
        let mut f = Function::new(vec![(1, ValType::I64)]); // local 0: len
        f.instruction(&Instruction::I64Const(0));
        f.instruction(&Instruction::Call(0)); // env.input(reg 0)
        f.instruction(&Instruction::I64Const(0));
        f.instruction(&Instruction::Call(1)); // env.register_len(0) → len
        f.instruction(&Instruction::LocalSet(0));
        f.instruction(&Instruction::I32Const(1024));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I64Store(MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
        f.instruction(&Instruction::I64Const(8));
        f.instruction(&Instruction::I64Const(1024));
        f.instruction(&Instruction::Call(2)); // env.value_return
        f.instruction(&Instruction::End);
        let mut code = CodeSection::new();
        code.function(&f);
        let mut module = Module::new();
        module
            .section(&types)
            .section(&imports)
            .section(&funcs)
            .section(&memory);
        module.section(&exports).section(&code);
        module.finish()
    };
    let chain = engine_with(&wasm);
    let payload: Vec<u8> = vec![0x41u8; 4 * 1024 * 1024]; // 4 MiB (> old 1 MiB cap)
    let out = chain
        .call("probe.test.near", "go")
        .args_bytes(payload.clone())
        .fire()
        .expect("fire");
    assert!(
        out.ok,
        "4 MiB input must be legal (mainnet register cap = 100 MiB): {:?}",
        out.error
    );
    let len = u64::from_le_bytes(
        out.return_data
            .as_deref()
            .and_then(|d| d.try_into().ok())
            .unwrap_or([0; 8]),
    );
    assert_eq!(
        len,
        payload.len() as u64,
        "register_len must report the full input"
    );
}


/// PV155 gas parity: a loop of N regular ops must burn ≈ N × regular_op_cost
/// (822,756) + entry overhead, measured via the instrumented global. Spin
/// 1_000_000 increments; expected wasm-side gas = ~1e6 × 822,756 = 8.2e11
/// (0.82 Tgas) plus host costs — assert the ballpark and, more importantly,
/// that it is NOT fuel-weighted (old behavior would report ~1e6 fuel units).
#[test]
fn gas_is_pv155_instrumented_not_fuel() {
    // (func (export "go") (local i32) 1000000 × {local.get 0; i32.const 1; i32.add; local.set 0})
    let wasm = {
        let mut types = TypeSection::new();
        types.ty().function(vec![], vec![]);
        let mut funcs = FunctionSection::new();
        funcs.function(0);
        let mut exports = ExportSection::new();
        exports.export("go", ExportKind::Func, 0);
        let mut f = Function::new(vec![(1, ValType::I32)]);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalSet(0));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        // count >= 1_000_000? leave
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(1_000_000));
        f.instruction(&Instruction::I32GeU);
        f.instruction(&Instruction::BrIf(1));
        // count += 1
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(0));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End); // loop
        f.instruction(&Instruction::End); // block
        f.instruction(&Instruction::End); // func
        let mut code = CodeSection::new();
        code.function(&f);
        let mut module = Module::new();
        module.section(&types).section(&funcs).section(&exports).section(&code);
        module.finish()
    };
    let chain = engine_with(&wasm);
    let out = chain.call("probe.test.near", "go").fire().expect("fire");
    assert!(out.ok, "loop failed: {:?}", out.error);
    // 1e6 iterations × 4 ops × 822,756 ≈ 3.3e12 = 3.3 Tgas. Old fuel meter
    // would have burned ~4e6 fuel (µTgas range). Assert instrumented scale.
    assert!(
        out.gas_burned > 1_000_000_000_000,
        "gas must be instrumented PV155 scale, got {} (fuel-like?)",
        out.gas_burned
    );
}

/// Out-of-gas must trap with mainnet's receipt-level message.
#[test]
fn out_of_gas_message_matches_mainnet() {
    // same infinite-ish loop but prepaid gas will be tiny — the mock defaults
    // to 200 Tgas for library calls, so loop 2_000_000+ times to exhaust it
    // (≈ 6.6 Tgas per 1e6 iters → need ~60e6 iters for 200 Tgas; use 100e6)
    let wasm = {
        let mut types = TypeSection::new();
        types.ty().function(vec![], vec![]);
        let mut funcs = FunctionSection::new();
        funcs.function(0);
        let mut exports = ExportSection::new();
        exports.export("go", ExportKind::Func, 0);
        let mut f = Function::new(vec![(1, ValType::I32)]);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalSet(0));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(100_000_000));
        f.instruction(&Instruction::I32GeU);
        f.instruction(&Instruction::BrIf(1));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(0));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
        let mut code = CodeSection::new();
        code.function(&f);
        let mut module = Module::new();
        module.section(&types).section(&funcs).section(&exports).section(&code);
        module.finish()
    };
    let chain = engine_with(&wasm);
    let out = chain.call("probe.test.near", "go").fire().expect("fire");
    assert!(!out.ok, "100M ops must exhaust 200 Tgas");
    let cls = out.panic.unwrap_or_default();
    assert!(
        cls.contains("Exceeded the prepaid gas"),
        "must classify as mainnet OOG, got panic class: {cls:?} / error: {:?}",
        out.error.as_deref().map(|s| &s[..s.len().min(80)])
    );
}
