//! Sponsor keys and fee-bump signing.
//!
//! The service only ever signs the *outer* fee-bump envelope. It never holds
//! user keys and cannot alter the inner transaction: the inner envelope, its
//! signatures and its hash are carried through untouched.

mod slip10;

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use stellar_xdr as xdr;
use xdr::{Limits, WriteXdr};

pub use slip10::{derive_ed25519, StellarPath};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid secret seed")]
    InvalidSecret,
    #[error("xdr: {0}")]
    Xdr(String),
    #[error("fee too low: need at least {needed} stroops")]
    FeeTooLow { needed: i64 },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Well-known network passphrases.
pub const PUBLIC_NETWORK: &str = "Public Global Stellar Network ; September 2015";
pub const TESTNET: &str = "Test SDF Network ; September 2015";

pub fn network_id(passphrase: &str) -> xdr::Hash {
    xdr::Hash(Sha256::digest(passphrase.as_bytes()).into())
}

/// An Ed25519 keypair that can act as a sponsor / fee source.
#[derive(Clone)]
pub struct Sponsor {
    key: SigningKey,
}

impl Sponsor {
    pub fn from_seed_bytes(seed: [u8; 32]) -> Self {
        Self {
            key: SigningKey::from_bytes(&seed),
        }
    }

    /// Accepts an `S...` strkey secret seed.
    pub fn from_strkey(secret: &str) -> Result<Self> {
        let parsed = stellar_strkey::ed25519::PrivateKey::from_string(secret)
            .map_err(|_| Error::InvalidSecret)?;
        Ok(Self::from_seed_bytes(parsed.0))
    }

    /// SEP-0005 account at `m/44'/148'/index'` of a BIP-39 seed.
    pub fn from_bip39_seed(seed: &[u8], index: u32) -> Self {
        Self::from_seed_bytes(derive_ed25519(seed, &StellarPath::account(index)))
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// `G...` strkey address.
    pub fn address(&self) -> String {
        stellar_strkey::ed25519::PublicKey(self.public_key_bytes())
            .to_string()
            .as_str()
            .to_owned()
    }

    pub fn muxed_account(&self) -> xdr::MuxedAccount {
        xdr::MuxedAccount::Ed25519(xdr::Uint256(self.public_key_bytes()))
    }

    pub fn account_id(&self) -> xdr::AccountId {
        xdr::AccountId(xdr::PublicKey::PublicKeyTypeEd25519(xdr::Uint256(
            self.public_key_bytes(),
        )))
    }

    fn sign_hash(&self, hash: &[u8; 32]) -> xdr::DecoratedSignature {
        let sig = self.key.sign(hash);
        let pk = self.public_key_bytes();
        let hint = xdr::SignatureHint([pk[28], pk[29], pk[30], pk[31]]);
        xdr::DecoratedSignature {
            hint,
            signature: xdr::Signature(sig.to_bytes().to_vec().try_into().expect("64 bytes")),
        }
    }

    /// Wrap an already-signed inner transaction in a fee-bump this sponsor pays for.
    ///
    /// The fee must cover the network minimum for every inner operation *plus
    /// one*, and must not be lower than the inner transaction's own fee.
    pub fn wrap_fee_bump(
        &self,
        inner: xdr::TransactionV1Envelope,
        base_fee_stroops: i64,
        passphrase: &str,
    ) -> Result<xdr::TransactionEnvelope> {
        let ops = (inner.tx.operations.len() as i64).max(1);
        // A fee bump has to pay for one operation more than the inner
        // transaction, at the inner transaction's own rate — not only at the
        // network minimum.
        let inclusion = inner.tx.fee as i64;
        let per_op = (inclusion + ops - 1) / ops;
        let needed = (inner.tx.fee as i64)
            .checked_add(per_op.max(base_fee_stroops))
            .ok_or(Error::FeeTooLow { needed: i64::MAX })?
            .max(base_fee_stroops * (ops + 1));
        let fee_bump = xdr::FeeBumpTransaction {
            fee_source: self.muxed_account(),
            fee: needed,
            inner_tx: xdr::FeeBumpTransactionInnerTx::Tx(inner),
            ext: xdr::FeeBumpTransactionExt::V0,
        };
        let payload = xdr::TransactionSignaturePayload {
            network_id: network_id(passphrase),
            tagged_transaction: xdr::TransactionSignaturePayloadTaggedTransaction::TxFeeBump(
                fee_bump.clone(),
            ),
        };
        let bytes = payload
            .to_xdr(Limits::none())
            .map_err(|e| Error::Xdr(e.to_string()))?;
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        let signature = self.sign_hash(&hash);
        Ok(xdr::TransactionEnvelope::TxFeeBump(
            xdr::FeeBumpTransactionEnvelope {
                tx: fee_bump,
                signatures: vec![signature]
                    .try_into()
                    .map_err(|_| Error::Xdr("sigs".into()))?,
            },
        ))
    }

    /// Add this sponsor's signature to an already-signed transaction envelope.
    ///
    /// Used only in bootstrap mode, where the sponsor is the transaction source
    /// and the new account co-signs `endSponsoringFutureReserves`. The user's
    /// signatures are preserved untouched.
    pub fn co_sign(
        &self,
        mut envelope: xdr::TransactionV1Envelope,
        passphrase: &str,
    ) -> Result<xdr::TransactionEnvelope> {
        let payload = xdr::TransactionSignaturePayload {
            network_id: network_id(passphrase),
            tagged_transaction: xdr::TransactionSignaturePayloadTaggedTransaction::Tx(
                envelope.tx.clone(),
            ),
        };
        let bytes = payload
            .to_xdr(Limits::none())
            .map_err(|e| Error::Xdr(e.to_string()))?;
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        let mut signatures = envelope.signatures.to_vec();
        signatures.push(self.sign_hash(&hash));
        envelope.signatures = signatures
            .try_into()
            .map_err(|_| Error::Xdr("too many signatures".into()))?;
        Ok(xdr::TransactionEnvelope::Tx(envelope))
    }

    /// Sign a plain (non fee-bump) transaction this sponsor is the source of.
    /// Used by the reserve-reclaim worker, never for user transactions.
    pub fn sign_transaction(
        &self,
        tx: xdr::Transaction,
        passphrase: &str,
    ) -> Result<xdr::TransactionEnvelope> {
        let payload = xdr::TransactionSignaturePayload {
            network_id: network_id(passphrase),
            tagged_transaction: xdr::TransactionSignaturePayloadTaggedTransaction::Tx(tx.clone()),
        };
        let bytes = payload
            .to_xdr(Limits::none())
            .map_err(|e| Error::Xdr(e.to_string()))?;
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        Ok(xdr::TransactionEnvelope::Tx(xdr::TransactionV1Envelope {
            tx,
            signatures: vec![self.sign_hash(&hash)]
                .try_into()
                .map_err(|_| Error::Xdr("sigs".into()))?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SEP-0005 test case 1: the 12-word mnemonic
    // "illness spike retreat truth genius clock brain pass fit cave bargain toe".
    const SEED_HEX: &str = "e4a5a632e70943ae7f07659df1332160937fad82587216a4c64315a0fb39497ee4a01f76ddab4cba68147977f3a147b6ad584c41808e8238a07f6cc4b582f186";

    #[test]
    fn sep5_derivation_matches_official_vectors() {
        let seed = hex::decode(SEED_HEX).unwrap();
        let expected = [
            "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6",
            "GBAW5XGWORWVFE2XTJYDTLDHXTY2Q2MO73HYCGB3XMFMQ562Q2W2GJQX",
            "GAY5PRAHJ2HIYBYCLZXTHID6SPVELOOYH2LBPH3LD4RUMXUW3DOYTLXW",
        ];
        for (i, want) in expected.iter().enumerate() {
            let sponsor = Sponsor::from_bip39_seed(&seed, i as u32);
            assert_eq!(&sponsor.address(), want, "account {i}");
        }
    }

    fn envelope_with(fee: u32, ops: usize, ext: xdr::TransactionExt) -> xdr::TransactionV1Envelope {
        let op = xdr::Operation {
            source_account: None,
            body: xdr::OperationBody::BumpSequence(xdr::BumpSequenceOp {
                bump_to: xdr::SequenceNumber(1),
            }),
        };
        xdr::TransactionV1Envelope {
            tx: xdr::Transaction {
                source_account: xdr::MuxedAccount::Ed25519(xdr::Uint256([1u8; 32])),
                fee,
                seq_num: xdr::SequenceNumber(1),
                cond: xdr::Preconditions::None,
                memo: xdr::Memo::None,
                operations: vec![op; ops].try_into().unwrap(),
                ext,
            },
            signatures: vec![].try_into().unwrap(),
        }
    }

    fn fee_of(envelope: xdr::TransactionEnvelope) -> i64 {
        match envelope {
            xdr::TransactionEnvelope::TxFeeBump(fb) => fb.tx.fee,
            _ => panic!("expected a fee bump"),
        }
    }

    #[test]
    fn fee_bump_pays_for_one_more_operation_at_the_inner_rate() {
        let sponsor = Sponsor::from_seed_bytes([3u8; 32]);
        // Classic: two operations bid at the network minimum.
        let classic = envelope_with(200, 2, xdr::TransactionExt::V0);
        assert_eq!(
            fee_of(sponsor.wrap_fee_bump(classic, 100, TESTNET).unwrap()),
            300
        );

        // A transaction bidding above the minimum keeps that rate.
        let eager = envelope_with(2_000, 2, xdr::TransactionExt::V0);
        assert_eq!(
            fee_of(sponsor.wrap_fee_bump(eager, 100, TESTNET).unwrap()),
            3_000
        );
    }

    #[test]
    fn strkey_round_trip() {
        let sponsor = Sponsor::from_seed_bytes([7u8; 32]);
        let key = stellar_strkey::ed25519::PrivateKey::from_payload(&[7u8; 32]).unwrap();
        let secret = stellar_strkey::Unredacted(&key).to_string();
        let secret = secret.as_str();
        assert_eq!(
            Sponsor::from_strkey(&secret).unwrap().address(),
            sponsor.address()
        );
    }
}
