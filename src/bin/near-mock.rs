//! `near-mock` binary — thin CLI over the `near_mock` library.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    near_mock::main_entry(std::env::args().collect::<Vec<_>>().as_slice())
}
