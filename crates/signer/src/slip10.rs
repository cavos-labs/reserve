//! SLIP-0010 ed25519 derivation, hardened-only, which is all SEP-0005 needs.

use hmac::{Hmac, Mac};
use sha2::Sha512;

type HmacSha512 = Hmac<Sha512>;

/// A hardened-only derivation path.
pub struct StellarPath(Vec<u32>);

impl StellarPath {
    /// `m/44'/148'/index'` — the SEP-0005 account path.
    pub fn account(index: u32) -> Self {
        StellarPath(vec![44, 148, index])
    }

    pub fn indices(&self) -> &[u32] {
        &self.0
    }
}

/// Derive the ed25519 private key for `path` from a BIP-39 seed.
pub fn derive_ed25519(seed: &[u8], path: &StellarPath) -> [u8; 32] {
    let mut mac = HmacSha512::new_from_slice(b"ed25519 seed").expect("hmac key");
    mac.update(seed);
    let i = mac.finalize().into_bytes();
    let mut key: [u8; 32] = i[..32].try_into().unwrap();
    let mut chain: [u8; 32] = i[32..].try_into().unwrap();

    for index in path.indices() {
        let hardened = index | 0x8000_0000;
        let mut mac = HmacSha512::new_from_slice(&chain).expect("hmac key");
        mac.update(&[0u8]);
        mac.update(&key);
        mac.update(&hardened.to_be_bytes());
        let i = mac.finalize().into_bytes();
        key = i[..32].try_into().unwrap();
        chain = i[32..].try_into().unwrap();
    }
    key
}
