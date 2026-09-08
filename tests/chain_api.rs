//! `MockChain` library-API tests. Each `#[test]` runs on its own thread, which
//! is exactly the MockChain model (engine state is thread-local).

use near_mock::chain::MockChain;

const GB: &str = "guestbook.test.near";

fn gb_chain() -> MockChain {
    MockChain::builder()
        .contract(GB, "fixtures/guestbook.wasm")
        .expect("fixture wasm")
        .signer("alice.test.near")
        .now(1_788_000_000)
        .build()
        .expect("build chain")
}

#[test]
fn tx_then_view_roundtrip() {
    let chain = gb_chain();
    assert_eq!(
        chain.storage_len(),
        1,
        "fresh chain = genesis validators only"
    );

    let tx = chain
        .call(GB, "sign")
        .args(r#"{"message":"hello lib"}"#)
        .fire()
        .expect("fire");
    assert!(tx.ok, "sign should succeed: {:?}", tx.error);
    assert!(!tx.entry_trapped);
    assert_eq!(tx.orphan_failures, 0);
    assert!(tx.gas_burned > 0, "gas must be metered");
    assert_eq!(
        chain.storage_len(),
        2,
        "STATE + near-bal(0)? no: STATE + signer list"
    );

    let view = chain.view(GB, "get_signature_count").fire().expect("fire");
    assert!(view.ok);
    assert_eq!(view.return_string().as_deref(), Some("1"));

    let list = chain.view(GB, "get_signatures").fire().expect("fire");
    assert!(list.ok);
    assert_eq!(
        list.return_string().as_deref(),
        Some("1"),
        "fixture's get_signatures returns the count"
    );
    // Ground truth: the STATE blob records the message + signer.
    let state_blob = chain.storage_get(GB, b"STATE").expect("STATE key");
    let s = String::from_utf8_lossy(&state_blob);
    assert!(s.contains("hello lib"), "message recorded, got: {s}");
    assert!(s.contains("alice.test.near"), "signer recorded, got: {s}");
}

#[test]
fn trapped_tx_is_atomic() {
    let chain = gb_chain();
    // near-sdk guest panics on unparsable JSON args → guest trap.
    let tx = chain
        .call(GB, "sign")
        .args("this is not json")
        .fire()
        .expect("fire returns outcome even on trap");
    assert!(!tx.ok, "malformed args must fail the tx");
    assert!(tx.entry_trapped, "failure is at the entry, not a receipt");
    assert!(tx.error.as_deref().unwrap_or_default().len() > 0);
    assert_eq!(
        chain.storage_len(),
        1,
        "trap rolls back to genesis (validators pre-exist; guest writes gone)"
    );
}

#[test]
fn attached_deposit_commits_and_refunds() {
    let chain = gb_chain();
    let deposit: u128 = 1_234_500_000;

    // Success: deposit credited to the callee before entry runs.
    let tx = chain
        .call(GB, "sign")
        .args(r#"{"message":"paid"}"#)
        .attach(deposit)
        .fire()
        .expect("fire");
    assert!(tx.ok);
    let bal = chain.storage_get(GB, b"\x00near-bal").expect("balance key");
    assert_eq!(
        std::str::from_utf8(&bal).unwrap().parse::<u128>().unwrap(),
        deposit
    );

    // Failure: deposit refunded (full rollback, snapshot excludes the credit).
    let tx2 = chain
        .call(GB, "sign")
        .args("not json")
        .attach(777)
        .fire()
        .expect("fire");
    assert!(!tx2.ok);
    let bal2 = chain.storage_get(GB, b"\x00near-bal").unwrap();
    assert_eq!(
        std::str::from_utf8(&bal2).unwrap().parse::<u128>().unwrap(),
        deposit,
        "failed tx must not keep its attach"
    );
}

#[test]
fn state_persists_and_reloads() {
    let path = format!("/tmp/nm_chain_test_{}.bin", std::process::id());
    let _ = std::fs::remove_file(&path);

    {
        let chain = MockChain::builder()
            .contract(GB, "fixtures/guestbook.wasm")
            .expect("fixture wasm")
            .state_path(&path)
            .build()
            .expect("build");
        let tx = chain
            .call(GB, "sign")
            .args(r#"{"message":"persist me"}"#)
            .fire()
            .expect("fire");
        assert!(tx.ok);
        chain.save().expect("save");
    }

    // Fresh chain (same thread is fine — build() reinstalls the TLS).
    let chain2 = MockChain::builder()
        .contract(GB, "fixtures/guestbook.wasm")
        .expect("fixture wasm")
        .state_path(&path)
        .build()
        .expect("rebuild");
    let view = chain2.view(GB, "get_signature_count").fire().expect("fire");
    assert_eq!(view.return_string().as_deref(), Some("1"), "state reloaded");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn deterministic_reruns_match_byte_for_byte() {
    let run = || {
        let chain = gb_chain();
        let tx = chain
            .call(GB, "sign")
            .args(r#"{"message":"det"}"#)
            .fire()
            .expect("fire");
        let view = chain.view(GB, "get_signatures").fire().expect("fire");
        (tx.ok, tx.gas_burned, view.return_string())
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "same inputs → identical outcome, gas, and output");
}
