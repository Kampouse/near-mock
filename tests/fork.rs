//! Fork-mode integration (network — run explicitly with --ignored):
//!   cargo test --release --test fork -- --ignored
//! Calls a real mainnet contract through the fork and compares against the
//! RPC's own view execution — byte-identical results are the contract here.

#[test]
#[ignore = "requires network + mainnet RPC"]
fn fork_view_matches_rpc() {
    use near_mock::chain::MockChain;
    let rpc = "https://rpc.mainnet.fastnear.com";
    let block = {
        // pin once for determinism
        let out = std::process::Command::new("curl")
            .args(["-s", "--max-time", "20", "-X", "POST",
                "-H", "Content-Type: application/json",
                "-d", r#"{"jsonrpc":"2.0","id":"d","method":"status","params":[null]}"#,
                rpc])
            .output().expect("curl");
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("status json");
        v.pointer("/result/sync_info/latest_block_height")
            .and_then(|h| h.as_u64())
            .expect("height")
    };
    let chain = MockChain::builder()
        .fork(rpc, Some(block))
        .signer("alice.test.near")
        .build()
        .expect("fork chain");

    // ground truth via RPC call_function at the SAME block
    let rpc_call = |method: &str, args: &str| -> String {
        use base64::Engine;
        let a = base64::engine::general_purpose::STANDARD.encode(args);
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": "d", "method": "query",
            "params": {"request_type": "call_function", "account_id": "omft.near",
                       "method_name": method, "args_base64": a, "block_id": block}
        });
        let out = std::process::Command::new("curl")
            .args(["-s", "--max-time", "20", "-X", "POST",
                "-H", "Content-Type: application/json",
                "-d", &body.to_string(), rpc])
            .output().expect("curl");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        let bytes = v.pointer("/result/result").and_then(|r| r.as_array()).expect("result");
        bytes.iter().filter_map(|b| b.as_u64().map(|x| x as u8)).collect::<Vec<u8>>()
            .iter().map(|b| *b as char).collect()
    };

    for (method, args) in [
        ("acl_is_super_admin", r#"{"account_id":"omft.near"}"#),
        ("contract_source_metadata", "{}"),
    ] {
        let expected = rpc_call(method, args);
        let out = chain
            .view("omft.near", method)
            .args(args.to_string())
            .fire()
            .expect("fire");
        assert!(out.ok, "{method} fork call failed: {:?}", out.error);
        let got = out.return_string().unwrap_or_default();
        assert_eq!(
            got, expected,
            "{method}: fork result must be byte-identical to RPC execution"
        );
    }
}
