//! # near-mock — local NEAR contract runner
//!
//! Deterministic, in-process NEAR contract execution: real contracts (wasm),
//! real host functions, real crypto, real gas schedule — no node, no
//! network, no consensus. Embed via [`main_entry`], or use the `near-mock`
//! binary.

pub mod near_mock;

pub use near_mock::main_entry;
