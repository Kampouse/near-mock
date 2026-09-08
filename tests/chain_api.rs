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
    // Regression (issue #1 L1): this view previously returned "1" — stale
    // data leaked from the get_signature_count call above, because
    // execute_tx never cleared return_data between library calls.
    assert_ne!(
        list.return_string().as_deref(),
        Some("1"),
        "get_signatures must not replay the previous call's return"
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

const SCEN: &str = "scen.test.near";

fn scen_chain() -> MockChain {
    MockChain::builder()
        .contract(SCEN, "fixtures/scen.wasm")
        .expect("fixture wasm")
        .signer("alice.test.near")
        .now(1_788_000_000)
        .build()
        .expect("build chain")
}

#[test]
fn no_stale_return_data_between_calls() {
    // Issue #1 L1: every call after the first returned the FIRST call's data.
    let chain = scen_chain();
    let v1 = chain.view(SCEN, "whoami").fire().expect("fire");
    assert!(v1.ok, "whoami failed: {:?}", v1.error);
    let first = v1.return_string();

    let v2 = chain.view(SCEN, "clock").fire().expect("fire");
    assert!(v2.ok, "clock failed: {:?}", v2.error);
    assert_ne!(
        v2.return_string(),
        first,
        "second view returned the first view's data (stale return_data leak)"
    );
}

#[test]
fn view_rejects_writes() {
    // Issue #1 L2: view() did not enforce read-only on the library path.
    let chain = scen_chain();
    let before = chain.storage_len();
    let v = chain.view(SCEN, "gate").fire().expect("fire");
    assert!(
        !v.ok,
        "a write inside a view must fail; got ok with data {:?}",
        v.return_string()
    );
    assert_eq!(
        chain.storage_len(),
        before,
        "view must not change storage count"
    );
    // And the write must not have leaked the 'breach' key in.
    assert!(
        chain.storage_get(SCEN, b"breach").is_none(),
        "view committed the write anyway"
    );
}
