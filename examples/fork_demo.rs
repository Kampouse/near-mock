//! Fork-mode library demo: call a real mainnet contract's views through
//! MockChain with lazy state — results must match RPC byte-for-byte.
use near_mock::chain::MockChain;
fn main() {
    let chain = MockChain::builder()
        .fork("https://rpc.mainnet.fastnear.com", None) // pin latest at build
        .signer("alice.test.near")
        .build()
        .expect("fork chain");
    // two different readers of real state:
    for acct in ["omft.near", "defuse-ops.near"] {
        let out = chain
            .view("omft.near", "acl_is_super_admin")
            .args(format!(r#"{{"account_id":"{acct}"}}"#))
            .fire()
            .expect("fire");
        println!(
            "{acct}: ok={} → {}",
            out.ok,
            out.return_string().unwrap_or_default()
        );
    }
}
