//! The submit-time gate.
//!
//! Rather than allowlisting operation types and hoping the list is complete,
//! the service rebuilds the transaction it quoted and demands an exact XDR
//! match. Anything the client changed — an extra operation, a lowered
//! `destAmount`, a different destination, a bumped sequence — fails here.

use stellar_xdr as xdr;
use xdr::{Limits, ReadXdr};

use crate::build::{build_inner, tx_to_xdr};
use crate::quote::Quote;
use crate::{Error, Result};

/// Parse a signed inner transaction envelope and check it against the quote.
///
/// Returns the envelope ready to be fee-bumped.
pub fn check_signed_inner(
    signed_xdr_base64: &str,
    quote: &Quote,
    current_ledger: u32,
) -> Result<xdr::TransactionV1Envelope> {
    quote.check_fresh(current_ledger)?;

    let envelope = xdr::TransactionEnvelope::from_xdr_base64(signed_xdr_base64, Limits::none())
        .map_err(|e| Error::Xdr(e.to_string()))?;
    let envelope = match envelope {
        xdr::TransactionEnvelope::Tx(v1) => v1,
        // A client-supplied fee bump would put someone else's account on the
        // outside of our envelope; we build that part ourselves.
        _ => return Err(Error::Mismatch("expected a v1 transaction envelope".into())),
    };

    if envelope.signatures.is_empty() {
        return Err(Error::Mismatch("transaction is unsigned".into()));
    }

    let expected = build_inner(quote)?;
    let got = tx_to_xdr(&envelope.tx)?;
    if got != tx_to_xdr(&expected)? {
        return Err(Error::Mismatch(
            "transaction body differs from the quoted one".into(),
        ));
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Mode, UserOp};
    use xdr::WriteXdr;

    const USER: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";
    const SPONSOR: &str = "GBAW5XGWORWVFE2XTJYDTLDHXTY2Q2MO73HYCGB3XMFMQ562Q2W2GJQX";

    fn quote() -> Quote {
        Quote {
            network: "Test SDF Network ; September 2015".into(),
            mode: Mode::Sponsored,
            source: USER.into(),
            sponsor: SPONSOR.into(),
            channel: String::new(),
            ops: vec![UserOp::Payment {
                destination: SPONSOR.into(),
                asset: format!("USDC:{SPONSOR}"),
                amount: "1".into(),
            }],
            fee_token: format!("USDC:{SPONSOR}"),
            charge_stroops: 360,
            send_max_stroops: 303,
            path: vec![],
            reserve_stroops: 0,
            sequence: 43,
            inner_fee_stroops: 300,
            min_time: 0,
            max_time: 1_800_000_000,
            expires_at_ledger: 120,
        }
    }

    /// A signature the service never verifies itself — stellar-core does. It
    /// only has to be present.
    fn dummy_signature() -> xdr::DecoratedSignature {
        xdr::DecoratedSignature {
            hint: xdr::SignatureHint([0; 4]),
            signature: xdr::Signature(vec![0u8; 64].try_into().unwrap()),
        }
    }

    fn envelope_of(tx: xdr::Transaction) -> String {
        xdr::TransactionEnvelope::Tx(xdr::TransactionV1Envelope {
            tx,
            signatures: vec![dummy_signature()].try_into().unwrap(),
        })
        .to_xdr_base64(Limits::none())
        .unwrap()
    }

    #[test]
    fn accepts_the_transaction_it_built() {
        let q = quote();
        let signed = envelope_of(build_inner(&q).unwrap());
        assert!(check_signed_inner(&signed, &q, 100).is_ok());
    }

    #[test]
    fn rejects_a_lowered_fee_payment() {
        let q = quote();
        let mut tx = build_inner(&q).unwrap();
        let mut ops = tx.operations.to_vec();
        if let xdr::OperationBody::PathPaymentStrictReceive(p) = &mut ops[1].body {
            p.dest_amount = 1;
        }
        tx.operations = ops.try_into().unwrap();
        let err = check_signed_inner(&envelope_of(tx), &q, 100).unwrap_err();
        assert!(matches!(err, Error::Mismatch(_)));
    }

    #[test]
    fn rejects_an_injected_operation() {
        let q = quote();
        let mut tx = build_inner(&q).unwrap();
        let mut ops = tx.operations.to_vec();
        ops.push(xdr::Operation {
            source_account: None,
            body: xdr::OperationBody::BumpSequence(xdr::BumpSequenceOp {
                bump_to: xdr::SequenceNumber(9_999),
            }),
        });
        tx.operations = ops.try_into().unwrap();
        let err = check_signed_inner(&envelope_of(tx), &q, 100).unwrap_err();
        assert!(matches!(err, Error::Mismatch(_)));
    }

    #[test]
    fn rejects_a_rewritten_destination() {
        let q = quote();
        let mut tx = build_inner(&q).unwrap();
        let mut ops = tx.operations.to_vec();
        if let xdr::OperationBody::Payment(p) = &mut ops[0].body {
            p.destination = crate::asset::muxed_account(USER).unwrap();
        }
        tx.operations = ops.try_into().unwrap();
        assert!(check_signed_inner(&envelope_of(tx), &q, 100).is_err());
    }

    #[test]
    fn rejects_unsigned_and_expired_submissions() {
        let q = quote();
        let unsigned = xdr::TransactionEnvelope::Tx(xdr::TransactionV1Envelope {
            tx: build_inner(&q).unwrap(),
            signatures: vec![].try_into().unwrap(),
        })
        .to_xdr_base64(Limits::none())
        .unwrap();
        assert!(matches!(
            check_signed_inner(&unsigned, &q, 100).unwrap_err(),
            Error::Mismatch(_)
        ));

        let signed = envelope_of(build_inner(&q).unwrap());
        assert!(matches!(
            check_signed_inner(&signed, &q, 121).unwrap_err(),
            Error::QuoteExpired { .. }
        ));
    }

    #[test]
    fn rejects_a_client_supplied_fee_bump() {
        let q = quote();
        let inner = xdr::TransactionV1Envelope {
            tx: build_inner(&q).unwrap(),
            signatures: vec![dummy_signature()].try_into().unwrap(),
        };
        let bumped = xdr::TransactionEnvelope::TxFeeBump(xdr::FeeBumpTransactionEnvelope {
            tx: xdr::FeeBumpTransaction {
                fee_source: crate::asset::muxed_account(USER).unwrap(),
                fee: 1_000,
                inner_tx: xdr::FeeBumpTransactionInnerTx::Tx(inner),
                ext: xdr::FeeBumpTransactionExt::V0,
            },
            signatures: vec![dummy_signature()].try_into().unwrap(),
        })
        .to_xdr_base64(Limits::none())
        .unwrap();
        assert!(check_signed_inner(&bumped, &q, 100).is_err());
    }
}
