//! Does decoding attacker-supplied XDR without a depth limit crash the process?
//!
//! ScVal nests recursively (a vector of ScVal), and the decoder recurses with
//! it. `Limits::none()` sets the depth to u32::MAX, and the crate's own
//! documentation says that limit exists to stop the program hitting Rust's
//! stack size, "which would result in an unrecoverable SIGABRT".
//!
//! Run: cargo run -p reserve-api --example depth_probe -- [none|limited]
use stellar_xdr::{Limits, ReadXdr, ScVal};

/// `depth` nested one-element vectors around a void. Twelve bytes per level.
fn nested(depth: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(depth * 12 + 4);
    for _ in 0..depth {
        bytes.extend_from_slice(&16u32.to_be_bytes()); // ScValType::Vec
        bytes.extend_from_slice(&1u32.to_be_bytes()); // Option: present
        bytes.extend_from_slice(&1u32.to_be_bytes()); // one element
    }
    bytes.extend_from_slice(&1u32.to_be_bytes()); // ScValType::Void
    bytes
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "none".into());
    let depth: usize = std::env::args()
        .nth(2)
        .and_then(|d| d.parse().ok())
        .unwrap_or(200_000);
    let payload = nested(depth);
    println!("{mode}: {depth} levels, {} bytes of payload", payload.len());

    let limits = if mode == "limited" {
        Limits {
            depth: 500,
            len: 5_000_000,
        }
    } else {
        Limits::none()
    };
    match ScVal::from_xdr(&payload, limits) {
        Ok(_) => println!("  decoded"),
        Err(e) => println!("  refused cleanly: {e}"),
    }
}
