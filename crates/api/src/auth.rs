//! Proving control of a Stellar account, to be handed a key.
//!
//! Shaped after SEP-10: the challenge is a transaction with sequence number
//! zero, which the network can never accept, carrying one `ManageData`
//! operation sourced by the account being proved. Every wallet can sign a
//! transaction, including through WalletConnect, which only exposes
//! `stellar_signXDR`; message signing is not available everywhere.
//!
//! It is not SEP-10 compliant, and does not try to be. SEP-10 is mutual: the
//! server signs the challenge so the client knows who it came from. Here the
//! client is served the page and the challenge over TLS by the same host, and
//! the only thing a forged challenge could obtain is a rate-limit bucket. The
//! challenge carries its own MAC instead, so the service stores no nonces.

use ed25519_dalek::{Signature, VerifyingKey};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use stellar_xdr as xdr;
use xdr::{Limits, ReadXdr, WriteXdr};

type HmacSha256 = Hmac<Sha256>;

/// How long a challenge is good for. Long enough to read it in a wallet,
/// short enough that a captured one is worthless.
const VALID_FOR: u64 = 300;
const DATA_NAME: &str = "reserve auth";

#[derive(Debug, PartialEq, Eq)]
pub enum Invalid {
    Malformed(&'static str),
    Expired,
    BadSignature,
}

fn mac(key: &[u8], address: &str, expires_at: u64) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    m.update(address.as_bytes());
    m.update(&expires_at.to_be_bytes());
    m.finalize().into_bytes().into()
}

/// Build the transaction the caller has to sign to prove they hold `address`.
pub fn challenge(address: &str, key: &[u8], now: u64) -> Result<String, Invalid> {
    let account = reserve_core::asset::account_id(address)
        .map_err(|_| Invalid::Malformed("not a Stellar address"))?;
    let expires_at = now + VALID_FOR;

    // The value carries its own expiry and MAC, so nothing has to be
    // remembered between the two requests.
    let mut value = expires_at.to_be_bytes().to_vec();
    value.extend_from_slice(&mac(key, address, expires_at));

    let tx = xdr::Transaction {
        // Sourced by the account being proved, so a wallet shows the user
        // their own account and nothing else can be done with the signature.
        source_account: xdr::MuxedAccount::Ed25519(match &account.0 {
            xdr::PublicKey::PublicKeyTypeEd25519(k) => k.clone(),
        }),
        fee: 0,
        // Zero means the network can never apply this transaction.
        seq_num: xdr::SequenceNumber(0),
        cond: xdr::Preconditions::Time(xdr::TimeBounds {
            min_time: xdr::TimePoint(now),
            max_time: xdr::TimePoint(expires_at),
        }),
        memo: xdr::Memo::None,
        operations: vec![xdr::Operation {
            source_account: None,
            body: xdr::OperationBody::ManageData(xdr::ManageDataOp {
                data_name: xdr::String64(
                    DATA_NAME
                        .as_bytes()
                        .to_vec()
                        .try_into()
                        .map_err(|_| Invalid::Malformed("data name"))?,
                ),
                data_value: Some(xdr::DataValue(
                    value
                        .try_into()
                        .map_err(|_| Invalid::Malformed("data value"))?,
                )),
            }),
        }]
        .try_into()
        .map_err(|_| Invalid::Malformed("operations"))?,
        ext: xdr::TransactionExt::V0,
    };

    xdr::TransactionEnvelope::Tx(xdr::TransactionV1Envelope {
        tx,
        signatures: vec![].try_into().expect("empty signatures"),
    })
    .to_xdr_base64(Limits::none())
    .map_err(|_| Invalid::Malformed("xdr"))
}

/// Check a signed challenge and return the address it proves.
pub fn verify(
    signed_xdr: &str,
    key: &[u8],
    network_passphrase: &str,
    now: u64,
) -> Result<String, Invalid> {
    let envelope = match xdr::TransactionEnvelope::from_xdr_base64(signed_xdr, Limits::none())
        .map_err(|_| Invalid::Malformed("not a transaction envelope"))?
    {
        xdr::TransactionEnvelope::Tx(v1) => v1,
        _ => return Err(Invalid::Malformed("expected a v1 envelope")),
    };
    let tx = &envelope.tx;

    if tx.seq_num.0 != 0 {
        return Err(Invalid::Malformed("a challenge has sequence zero"));
    }
    if tx.operations.len() != 1 {
        return Err(Invalid::Malformed("a challenge has one operation"));
    }
    let value = match &tx.operations[0].body {
        xdr::OperationBody::ManageData(op) => {
            if op.data_name.0.as_slice() != DATA_NAME.as_bytes() {
                return Err(Invalid::Malformed("not our challenge"));
            }
            op.data_value
                .as_ref()
                .ok_or(Invalid::Malformed("no value"))?
        }
        _ => return Err(Invalid::Malformed("a challenge manages data")),
    };

    let raw = value.0.as_slice();
    if raw.len() != 40 {
        return Err(Invalid::Malformed("value"));
    }
    let expires_at = u64::from_be_bytes(raw[..8].try_into().expect("checked"));

    let public_key = match &tx.source_account {
        xdr::MuxedAccount::Ed25519(k) => k.0,
        _ => return Err(Invalid::Malformed("muxed accounts cannot be proved")),
    };
    let address = stellar_strkey::ed25519::PublicKey(public_key)
        .to_string()
        .as_str()
        .to_owned();

    // Constant time, and before anything in the challenge is believed.
    let expected = mac(key, &address, expires_at);
    if raw[8..]
        .iter()
        .zip(expected.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        != 0
    {
        return Err(Invalid::Malformed("not our challenge"));
    }
    if now >= expires_at {
        return Err(Invalid::Expired);
    }

    // The signature has to be over this transaction on this network.
    let payload = xdr::TransactionSignaturePayload {
        network_id: xdr::Hash(Sha256::digest(network_passphrase.as_bytes()).into()),
        tagged_transaction: xdr::TransactionSignaturePayloadTaggedTransaction::Tx(tx.clone()),
    };
    let hash: [u8; 32] = Sha256::digest(
        payload
            .to_xdr(Limits::none())
            .map_err(|_| Invalid::Malformed("xdr"))?,
    )
    .into();
    let verifying = VerifyingKey::from_bytes(&public_key).map_err(|_| Invalid::BadSignature)?;
    let signed = envelope.signatures.iter().any(|s| {
        <[u8; 64]>::try_from(s.signature.0.as_slice())
            .ok()
            .is_some_and(|bytes| {
                verifying
                    .verify_strict(&hash, &Signature::from_bytes(&bytes))
                    .is_ok()
            })
    });
    if !signed {
        return Err(Invalid::BadSignature);
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reserve_signer::Sponsor;

    const NETWORK: &str = "Test SDF Network ; September 2015";
    const KEY: &[u8] = b"a server key that is long enough";

    fn signed_by(holder: &Sponsor, challenge: &str) -> String {
        let tx = match xdr::TransactionEnvelope::from_xdr_base64(challenge, Limits::none()).unwrap()
        {
            xdr::TransactionEnvelope::Tx(v1) => v1.tx,
            _ => unreachable!(),
        };
        holder
            .sign_transaction(tx, NETWORK)
            .unwrap()
            .to_xdr_base64(Limits::none())
            .unwrap()
    }

    #[test]
    fn proves_the_account_that_signed() {
        let holder = Sponsor::from_seed_bytes([1u8; 32]);
        let challenge = challenge(&holder.address(), KEY, 1_000).unwrap();
        assert_eq!(
            verify(&signed_by(&holder, &challenge), KEY, NETWORK, 1_100).unwrap(),
            holder.address()
        );
    }

    #[test]
    fn refuses_somebody_else_signing_it() {
        let holder = Sponsor::from_seed_bytes([1u8; 32]);
        let impostor = Sponsor::from_seed_bytes([2u8; 32]);
        let challenge = challenge(&holder.address(), KEY, 1_000).unwrap();
        assert_eq!(
            verify(&signed_by(&impostor, &challenge), KEY, NETWORK, 1_100),
            Err(Invalid::BadSignature)
        );
    }

    #[test]
    fn refuses_a_challenge_we_did_not_issue() {
        let holder = Sponsor::from_seed_bytes([1u8; 32]);
        let challenge = challenge(
            &holder.address(),
            b"somebody else's key aaaaaaaaaaaa",
            1_000,
        )
        .unwrap();
        assert_eq!(
            verify(&signed_by(&holder, &challenge), KEY, NETWORK, 1_100),
            Err(Invalid::Malformed("not our challenge"))
        );
    }

    #[test]
    fn refuses_an_expired_one() {
        let holder = Sponsor::from_seed_bytes([1u8; 32]);
        let challenge = challenge(&holder.address(), KEY, 1_000).unwrap();
        assert_eq!(
            verify(
                &signed_by(&holder, &challenge),
                KEY,
                NETWORK,
                1_000 + VALID_FOR
            ),
            Err(Invalid::Expired)
        );
    }

    #[test]
    fn refuses_a_signature_from_another_network() {
        let holder = Sponsor::from_seed_bytes([1u8; 32]);
        let challenge = challenge(&holder.address(), KEY, 1_000).unwrap();
        let signed = signed_by(&holder, &challenge);
        let elsewhere = "Public Global Stellar Network ; September 2015";
        assert_eq!(
            verify(&signed, KEY, elsewhere, 1_100),
            Err(Invalid::BadSignature)
        );
    }

    #[test]
    fn a_challenge_can_never_reach_the_network() {
        let holder = Sponsor::from_seed_bytes([1u8; 32]);
        let challenge = challenge(&holder.address(), KEY, 1_000).unwrap();
        let tx =
            match xdr::TransactionEnvelope::from_xdr_base64(&challenge, Limits::none()).unwrap() {
                xdr::TransactionEnvelope::Tx(v1) => v1.tx,
                _ => unreachable!(),
            };
        assert_eq!(tx.seq_num.0, 0);
        assert_eq!(tx.fee, 0);
    }

    #[test]
    fn a_signature_scalar_at_the_group_order_is_rejected() {
        // RFC 8032: s must be in [0, L). `verify` used to accept some of these.
        const L: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9,
            0xde, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x10,
        ];
        let holder = Sponsor::from_seed_bytes([1u8; 32]);
        let key = VerifyingKey::from_bytes(&holder.public_key_bytes()).unwrap();
        let mut bytes = [0u8; 64];
        bytes[32..].copy_from_slice(&L);
        assert!(key
            .verify_strict(b"msg", &Signature::from_bytes(&bytes))
            .is_err());
    }
}
