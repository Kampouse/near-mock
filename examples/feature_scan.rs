//! Feature-usage scan of the tracked replay contracts.
//!
//! Question (parity audit follow-up): do the contracts we replay actually use
//! wasm features where mock ≠ mainnet semantics?
//!
//! Method — detection by refusal:
//!   1. baseline engine (current mock config, wide-arithmetic OFF):
//!      a module that COMPILES here provably contains no wide-arithmetic
//!      opcodes (they would fail compilation).
//!   2. restrictive engine (SIMD/multi-value/threads OFF = mainnet's
//!      accepted set): a module that compiles here uses no mainnet-rejected
//!      features. (Expected for anything deployed on mainnet — prepare
//!      rejected such modules at deploy time. Sanity check.)
//!   3. float usage: heuristic byte-scan of the f32/f64 opcode range
//!      (0x8B..=0xBF) — indicative only (immediates can collide).
//!
//! Usage: cargo run --release --example feature_scan -- [dir=wasm dir]

fn compile_with(cfg: wasmtime::Config, bytes: &[u8]) -> Result<(), String> {
    let engine = wasmtime::Engine::new(&cfg).map_err(|e| e.to_string())?;
    wasmtime::Module::from_binary(&engine, bytes)
        .map(|_| ())
        .map_err(|e| e.to_string().chars().take(100).collect())
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/replay-wasms".to_string());
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("wasm dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wasm"))
        .collect();
    paths.sort();

    let mut restrictive = wasmtime::Config::new();
    restrictive.wasm_simd(false); // mainnet: SIMD=false
    restrictive.wasm_relaxed_simd(false); // (must go with simd off)
    restrictive.wasm_multi_value(false); // mainnet: MULTI_VALUE=false
    restrictive.wasm_threads(false); // mainnet: THREADS=false

    println!(
        "{:<38} {:>10} {:>10} {:>8}",
        "contract", "wide-arith", "rej-feat", "floats?"
    );
    println!("{}", "-".repeat(72));
    for p in &paths {
        let bytes = std::fs::read(p).unwrap();
        let name = p.file_name().unwrap().to_string_lossy();
        let name: String = name.chars().take(36).collect();

        // 1. baseline (wide-arith off): compiling ⇒ no wide-arithmetic used
        let wide = compile_with(near_mock_has_none(), &bytes);
        let wide_free = wide.is_ok();

        // 2. restrictive: compiling ⇒ no mainnet-rejected features
        let rej = compile_with(restrictive.clone(), &bytes);
        let rej_free = rej.is_ok();

        // 3. float byte-scan (heuristic)
        let floats = bytes
            .iter()
            .filter(|b| (**b as u16) >= 0x8B && (**b as u16) <= 0xBF)
            .count();

        println!(
            "{:<38} {:>10} {:>10} {:>8}",
            name,
            if wide_free { "none ✓" } else { "USED!" },
            if rej_free { "none ✓" } else { "USED!" },
            if floats > 0 {
                format!("{floats}B~")
            } else {
                "0".to_string()
            }
        );
        if let Err(e) = &rej {
            println!("    └ rejected-feature detail: {e}");
        }
    }
}

// baseline = current mock engine (fuel+NaN-canon, wide-arithmetic left at the
// wasmtime default: OFF). Reuse the library's own config for honesty.
fn near_mock_has_none() -> wasmtime::Config {
    // base_engine_config is pub(crate); replicate minimal equivalent here
    let mut cfg = wasmtime::Config::new();
    cfg.consume_fuel(true);
    cfg.cranelift_nan_canonicalization(true);
    cfg
}
