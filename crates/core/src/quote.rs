//! Quotes are not stored. They travel as a payload signed with the server's
//! HMAC key, so the hot path does no database writes and the API stays
//! stateless. `submit` verifies the signature, checks expiry, and rebuilds the
//! transaction from exactly these fields.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::plan::{Mode, UserOp};
use crate::{Error, Result};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Quote {
    pub network: String,
    pub mode: Mode,
    /// The account the operations are for.
    pub source: String,
    /// The sponsor that will carry reserves and pay the network fee.
    pub sponsor: String,
    /// Transaction source in bootstrap. Empty means the sponsor itself, which
    /// is the single-lane default. A distinct address is a channel account:
    /// it consumes the sequence, the sponsor still opens the sandwich.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
    pub ops: Vec<UserOp>,
    /// Canonical asset string the user pays us in.
    pub fee_token: String,
    /// XLM the sponsor receives (`destAmount` of the path payment).
    pub charge_stroops: i64,
    /// Cap on what leaves the user's balance (`sendMax`).
    pub send_max_stroops: i64,
    /// Route for the path payment, as canonical asset strings.
    pub path: Vec<String>,
    /// XLM this locks up in reserves, for accounting after submit.
    pub reserve_stroops: i64,
    /// Sequence number the inner transaction must carry.
    #[serde(with = "crate::numeric::stringly")]
    pub sequence: i64,
    /// Fee written into the inner transaction (the fee-bump pays the real one).
    pub inner_fee_stroops: u32,
    pub min_time: u64,
    pub max_time: u64,
    pub expires_at_ledger: u32,
}

impl Quote {
    /// `<payload>.<mac>`, both base64url.
    pub fn seal(&self, key: &[u8]) -> Result<String> {
        let payload = serde_json::to_vec(self).map_err(|e| Error::Xdr(e.to_string()))?;
        let mac = mac(key, &payload);
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode(mac)
        ))
    }

    pub fn open(token: &str, key: &[u8]) -> Result<Quote> {
        let (payload_b64, mac_b64) = token.split_once('.').ok_or(Error::QuoteSignature)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| Error::QuoteSignature)?;
        let given = URL_SAFE_NO_PAD
            .decode(mac_b64)
            .map_err(|_| Error::QuoteSignature)?;
        let expected = mac(key, &payload);
        // Constant-time comparison: a timing oracle here would let an attacker
        // forge quotes.
        if given.len() != expected.len()
            || given
                .iter()
                .zip(expected.iter())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                != 0
        {
            return Err(Error::QuoteSignature);
        }
        serde_json::from_slice(&payload).map_err(|_| Error::QuoteSignature)
    }

    /// Who sources the inner transaction. In bootstrap that is the channel
    /// (or the sponsor, when no pool was configured).
    pub fn tx_source(&self) -> &str {
        match self.mode {
            Mode::Sponsored => self.source.as_str(),
            Mode::Bootstrap if self.channel.is_empty() => self.sponsor.as_str(),
            Mode::Bootstrap => self.channel.as_str(),
        }
    }

    pub fn check_fresh(&self, current_ledger: u32) -> Result<()> {
        if current_ledger > self.expires_at_ledger {
            return Err(Error::QuoteExpired {
                expires_at_ledger: self.expires_at_ledger,
            });
        }
        Ok(())
    }
}

fn mac(key: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    m.update(payload);
    m.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";

    fn sample() -> Quote {
        Quote {
            network: "Test SDF Network ; September 2015".into(),
            mode: Mode::Sponsored,
            source: G.into(),
            sponsor: G.into(),
            channel: String::new(),
            ops: vec![UserOp::ChangeTrust {
                asset: format!("USDC:{G}"),
                limit: None,
            }],
            fee_token: format!("USDC:{G}"),
            charge_stroops: 360,
            send_max_stroops: 303,
            path: vec![],
            reserve_stroops: 5_000_000,
            sequence: 42,
            inner_fee_stroops: 400,
            min_time: 0,
            max_time: 1_800_000_000,
            expires_at_ledger: 120,
        }
    }

    #[test]
    fn round_trips_under_its_own_key() {
        let q = sample();
        let sealed = q.seal(b"secret").unwrap();
        assert_eq!(Quote::open(&sealed, b"secret").unwrap(), q);
    }

    #[test]
    fn rejects_tampering_and_foreign_keys() {
        let sealed = sample().seal(b"secret").unwrap();
        assert!(matches!(
            Quote::open(&sealed, b"other").unwrap_err(),
            Error::QuoteSignature
        ));

        // Flip a byte of the payload and re-attach the original mac.
        let (payload, mac) = sealed.split_once('.').unwrap();
        let mut raw = URL_SAFE_NO_PAD.decode(payload).unwrap();
        let last = raw.len() - 2;
        raw[last] ^= 0x01;
        let forged = format!("{}.{}", URL_SAFE_NO_PAD.encode(&raw), mac);
        assert!(matches!(
            Quote::open(&forged, b"secret").unwrap_err(),
            Error::QuoteSignature
        ));
    }

    #[test]
    fn bootstrap_tx_source_is_the_channel_or_the_sponsor() {
        let mut q = sample();
        q.mode = Mode::Bootstrap;
        assert_eq!(q.tx_source(), q.sponsor.as_str());
        q.channel = "GCHANNEL".into();
        assert_eq!(q.tx_source(), "GCHANNEL");
        q.mode = Mode::Sponsored;
        assert_eq!(q.tx_source(), q.source.as_str());
    }

    #[test]
    fn expiry_is_checked_against_the_ledger() {
        let q = sample();
        assert!(q.check_fresh(119).is_ok());
        assert!(q.check_fresh(120).is_ok());
        assert!(matches!(
            q.check_fresh(121).unwrap_err(),
            Error::QuoteExpired { .. }
        ));
    }
}
