//! Integrator keys.
//!
//! A key is an identity, never an authorisation: the service answers unkeyed
//! callers too, from a smaller budget. That is what keeps "paste the URL and
//! it works" true, and it is also why the key is cheap to get.
//!
//! A key is a signed payload, verified here offline. There is no revocation
//! list: a leaked key buys somebody a larger rate-limit bucket and nothing
//! else, which an expiry date bounds well enough. Two consequences of
//! verifying offline, both deliberate:
//!
//!   * the hot path makes no call to the issuer, so issuing being down cannot
//!     take this service down, and a self-hosted deployment needs nothing from
//!     anybody;
//!   * the signature is asymmetric, so this service only ever holds the public
//!     half. Compromising it, or any self-hosted copy, does not let anyone mint
//!     keys. A shared secret would have.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// What the issuer said about a key. Kept deliberately small: anything that
/// needs to change often does not belong in a token nobody can revoke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedKey {
    /// Identifies the key without revealing it. Safe to log.
    pub id: String,
    /// Network passphrase this key is good for, when the issuer scoped it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    /// Unix seconds. A key without one never expires, which is worth avoiding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Invalid {
    Malformed,
    BadSignature,
    Expired,
    WrongNetwork,
}

/// Mints keys. Only a deployment that issues its own needs one; a deployment
/// that merely accepts keys holds the public half and cannot mint.
pub struct KeyIssuer {
    signing: SigningKey,
}

impl KeyIssuer {
    /// `secret` is 32 bytes, hex encoded.
    pub fn new(secret: &str) -> Result<KeyIssuer, String> {
        let bytes: [u8; 32] = hex::decode(secret.trim())
            .map_err(|_| "issuer secret is not hex".to_string())?
            .try_into()
            .map_err(|_| "issuer secret must be 32 bytes".to_string())?;
        Ok(KeyIssuer {
            signing: SigningKey::from_bytes(&bytes),
        })
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    pub fn mint(&self, key: &IssuedKey) -> Result<String, String> {
        let payload = serde_json::to_vec(key).map_err(|e| e.to_string())?;
        let signature = self.signing.sign(&payload);
        Ok(format!(
            "cav_{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
}

pub struct KeyVerifier {
    issuer: Option<VerifyingKey>,
    network: String,
}

impl KeyVerifier {
    /// `issuer_public_key` is 32 bytes, hex encoded. Without one, no key is
    /// accepted and every caller is anonymous.
    pub fn new(
        issuer_public_key: Option<&str>,
        network: impl Into<String>,
    ) -> Result<KeyVerifier, String> {
        let issuer = match issuer_public_key {
            Some(hex_key) => {
                let bytes: [u8; 32] = hex::decode(hex_key.trim())
                    .map_err(|_| "issuer public key is not hex".to_string())?
                    .try_into()
                    .map_err(|_| "issuer public key must be 32 bytes".to_string())?;
                Some(VerifyingKey::from_bytes(&bytes).map_err(|e| e.to_string())?)
            }
            None => None,
        };
        Ok(KeyVerifier {
            issuer,
            network: network.into(),
        })
    }

    /// Verify `cav_<payload>.<signature>`, both base64url.
    pub fn verify(&self, token: &str, now: u64) -> Result<IssuedKey, Invalid> {
        let issuer = self.issuer.as_ref().ok_or(Invalid::BadSignature)?;
        let body = token.strip_prefix("cav_").ok_or(Invalid::Malformed)?;
        let (payload_b64, sig_b64) = body.split_once('.').ok_or(Invalid::Malformed)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| Invalid::Malformed)?;
        let signature: [u8; 64] = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| Invalid::Malformed)?
            .try_into()
            .map_err(|_| Invalid::Malformed)?;

        // Signature first: nothing inside the payload is trustworthy until the
        // issuer's signature over these exact bytes checks out.
        issuer
            .verify_strict(payload.as_slice(), &Signature::from_bytes(&signature))
            .map_err(|_| Invalid::BadSignature)?;

        let key: IssuedKey = serde_json::from_slice(&payload).map_err(|_| Invalid::Malformed)?;
        if key.expires_at.is_some_and(|at| now >= at) {
            return Err(Invalid::Expired);
        }
        if key.network.as_ref().is_some_and(|n| n != &self.network) {
            return Err(Invalid::WrongNetwork);
        }
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const NETWORK: &str = "Test SDF Network ; September 2015";

    fn issuer() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn mint(key: &IssuedKey) -> String {
        let payload = serde_json::to_vec(key).unwrap();
        let signature = issuer().sign(&payload);
        format!(
            "cav_{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    fn verifier() -> KeyVerifier {
        let public = hex::encode(issuer().verifying_key().to_bytes());
        KeyVerifier::new(Some(&public), NETWORK).unwrap()
    }

    fn sample() -> IssuedKey {
        IssuedKey {
            id: "acme".into(),
            network: Some(NETWORK.into()),
            expires_at: Some(2_000_000_000),
        }
    }

    #[test]
    fn what_an_issuer_mints_its_verifier_accepts() {
        let issuer = KeyIssuer::new(&hex::encode([7u8; 32])).unwrap();
        let verifier = KeyVerifier::new(Some(&issuer.public_key_hex()), NETWORK).unwrap();
        let token = issuer.mint(&sample()).unwrap();
        assert_eq!(verifier.verify(&token, 1_000).unwrap(), sample());
    }

    #[test]
    fn accepts_a_key_the_issuer_signed() {
        assert_eq!(
            verifier().verify(&mint(&sample()), 1_000).unwrap(),
            sample()
        );
    }

    #[test]
    fn refuses_a_key_nobody_signed() {
        // The payload is honest, the signature is not.
        let payload = serde_json::to_vec(&sample()).unwrap();
        let forged = format!(
            "cav_{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode([0u8; 64])
        );
        assert_eq!(
            verifier().verify(&forged, 1_000),
            Err(Invalid::BadSignature)
        );
    }

    #[test]
    fn refuses_a_payload_edited_after_signing() {
        let token = mint(&sample());
        let (head, sig) = token.split_once('.').unwrap();
        let mut payload = URL_SAFE_NO_PAD
            .decode(head.strip_prefix("cav_").unwrap())
            .unwrap();
        let last = payload.len() - 3;
        payload[last] ^= 0x01;
        let tampered = format!("cav_{}.{}", URL_SAFE_NO_PAD.encode(&payload), sig);
        assert!(matches!(
            verifier().verify(&tampered, 1_000),
            Err(Invalid::BadSignature) | Err(Invalid::Malformed)
        ));
    }

    #[test]
    fn refuses_a_key_signed_by_somebody_else() {
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let public = hex::encode(other.verifying_key().to_bytes());
        let verifier = KeyVerifier::new(Some(&public), NETWORK).unwrap();
        assert_eq!(
            verifier.verify(&mint(&sample()), 1_000),
            Err(Invalid::BadSignature)
        );
    }

    #[test]
    fn honours_expiry_and_scope() {
        assert_eq!(
            verifier().verify(&mint(&sample()), 2_000_000_000),
            Err(Invalid::Expired)
        );
        let mainnet = IssuedKey {
            network: Some("Public Global Stellar Network ; September 2015".into()),
            ..sample()
        };
        assert_eq!(
            verifier().verify(&mint(&mainnet), 1_000),
            Err(Invalid::WrongNetwork)
        );
    }

    #[test]
    fn without_an_issuer_configured_no_key_is_accepted() {
        let verifier = KeyVerifier::new(None, NETWORK).unwrap();
        assert_eq!(
            verifier.verify(&mint(&sample()), 1_000),
            Err(Invalid::BadSignature)
        );
    }
}
