//! Deterministic construction of the inner transaction.
//!
//! Determinism is the security property, not a nicety: `validate` rebuilds the
//! transaction from the quote and compares XDR with what came back signed, so
//! this function must produce the exact same bytes for the same quote.

use stellar_xdr as xdr;
use xdr::{Limits, WriteXdr};

use crate::asset::{account_id, muxed_account, parse_asset, to_change_trust_asset, to_xdr_asset};
use crate::plan::{Mode, UserOp};
use crate::quote::Quote;
use crate::{Error, Result};

/// Trustline limit used when the caller does not specify one: the XDR maximum,
/// which is what every wallet means by "no limit".
const MAX_TRUSTLINE_LIMIT: i64 = i64::MAX;

pub fn build_inner(quote: &Quote) -> Result<xdr::Transaction> {
    let user = &quote.source;
    let sponsor = &quote.sponsor;
    let needs_sponsorship = quote.reserve_stroops > 0;

    // In bootstrap the user's account does not exist yet, so it cannot source
    // the transaction. The channel does (the sponsor, when no pool is set).
    let tx_source = muxed_account(quote.tx_source())?;

    let mut ops: Vec<xdr::Operation> = Vec::new();

    if needs_sponsorship {
        ops.push(op(
            // CAP-33: the *operation* source is the sponsor, not the
            // transaction source. When the channel is the sponsor these are
            // the same account and the field stays empty so the bytes match
            // what we have always built.
            sponsorship_source(quote, sponsor)?,
            xdr::OperationBody::BeginSponsoringFutureReserves(
                xdr::BeginSponsoringFutureReservesOp {
                    sponsored_id: account_id(user)?,
                },
            ),
        ));
    }

    // In bootstrap a lane sources the transaction, so every operation that
    // acts on the user's own account has to name the user explicitly —
    // otherwise it would silently apply to the lane.
    let user_source = match quote.mode {
        Mode::Sponsored => None,
        Mode::Bootstrap => Some(muxed_account(user)?),
    };

    for user_op in &quote.ops {
        ops.push(match user_op {
            // The funding side of createAccount is always the transaction
            // source: the channel in bootstrap, the user paying for someone
            // else's account otherwise.
            UserOp::CreateAccount { destination } => op(
                None,
                xdr::OperationBody::CreateAccount(xdr::CreateAccountOp {
                    destination: account_id(destination)?,
                    // Zero: the reserves are sponsored, not funded.
                    starting_balance: 0,
                }),
            ),
            UserOp::Payment {
                destination,
                asset,
                amount,
            } => op(
                user_source.clone(),
                xdr::OperationBody::Payment(xdr::PaymentOp {
                    destination: muxed_account(destination)?,
                    asset: to_xdr_asset(&parse_asset(asset)?)?,
                    amount: parse_amount(amount)?,
                }),
            ),
            UserOp::PathPaymentStrictSend {
                destination,
                send_asset,
                send_amount,
                dest_asset,
                dest_min,
                path,
            } => op(
                user_source.clone(),
                xdr::OperationBody::PathPaymentStrictSend(xdr::PathPaymentStrictSendOp {
                    send_asset: to_xdr_asset(&parse_asset(send_asset)?)?,
                    send_amount: parse_amount(send_amount)?,
                    destination: muxed_account(destination)?,
                    dest_asset: to_xdr_asset(&parse_asset(dest_asset)?)?,
                    dest_min: parse_amount(dest_min)?,
                    path: path_assets(path)?,
                }),
            ),
            UserOp::ClaimBalance { balance_id } => op(
                user_source.clone(),
                xdr::OperationBody::ClaimClaimableBalance(xdr::ClaimClaimableBalanceOp {
                    balance_id: parse_balance_id(balance_id)?,
                }),
            ),
            UserOp::ChangeTrust { asset, limit } => op(
                user_source.clone(),
                xdr::OperationBody::ChangeTrust(xdr::ChangeTrustOp {
                    line: to_change_trust_asset(&parse_asset(asset)?)?,
                    limit: match limit {
                        Some(l) => parse_amount(l)?,
                        None => MAX_TRUSTLINE_LIMIT,
                    },
                }),
            ),
        });
    }

    if needs_sponsorship {
        // Closed by the sponsored account itself — this is what makes the
        // sponsorship mutually agreed, and why the user co-signs in bootstrap.
        ops.push(op(
            Some(muxed_account(user)?),
            xdr::OperationBody::EndSponsoringFutureReserves,
        ));
    }

    // Every transaction pays for itself now, bootstrap included: the funds
    // being claimed are right there to pay from.
    ops.push(fee_payment_op(quote)?);

    let operations: xdr::VecM<xdr::Operation, 100> = ops
        .try_into()
        .map_err(|_| Error::Unsupported("too many operations".into()))?;

    Ok(xdr::Transaction {
        source_account: tx_source,
        fee: quote.inner_fee_stroops,
        seq_num: xdr::SequenceNumber(quote.sequence),
        cond: xdr::Preconditions::Time(xdr::TimeBounds {
            min_time: xdr::TimePoint(quote.min_time),
            max_time: xdr::TimePoint(quote.max_time),
        }),
        memo: xdr::Memo::None,
        operations,
        ext: xdr::TransactionExt::V0,
    })
}

/// Horizon renders a claimable balance id as 72 hex characters: a four byte
/// type prefix followed by a 32 byte hash.
pub fn parse_balance_id(id: &str) -> Result<xdr::ClaimableBalanceId> {
    let bytes = hex::decode(id).map_err(|_| Error::Amount(format!("balance id {id}")))?;
    if bytes.len() != 36 || bytes[..4] != [0, 0, 0, 0] {
        return Err(Error::Amount(format!("balance id {id}")));
    }
    let hash: [u8; 32] = bytes[4..].try_into().expect("checked length");
    Ok(xdr::ClaimableBalanceId::ClaimableBalanceIdTypeV0(
        xdr::Hash(hash),
    ))
}

/// The user pays us by buying exactly `charge_stroops` of XLM with their token,
/// atomically, in the same transaction. If the route moves against them beyond
/// `send_max`, the whole transaction fails and nothing is charged.
fn fee_payment_op(quote: &Quote) -> Result<xdr::Operation> {
    let token = parse_asset(&quote.fee_token)?;
    let path = quote
        .path
        .iter()
        .map(|a| to_xdr_asset(&parse_asset(a)?))
        .collect::<Result<Vec<_>>>()?;
    // In bootstrap the transaction source is a lane, so the payment has
    // to name the account actually paying.
    let payer = match quote.mode {
        Mode::Sponsored => None,
        Mode::Bootstrap => Some(muxed_account(&quote.source)?),
    };
    Ok(op(
        payer,
        xdr::OperationBody::PathPaymentStrictReceive(xdr::PathPaymentStrictReceiveOp {
            send_asset: to_xdr_asset(&token)?,
            send_max: quote.send_max_stroops,
            destination: muxed_account(&quote.sponsor)?,
            dest_asset: xdr::Asset::Native,
            dest_amount: quote.charge_stroops,
            path: path
                .try_into()
                .map_err(|_| Error::Unsupported("path too long".into()))?,
        }),
    ))
}

/// Name the sponsor on `begin` whenever it is not already the transaction
/// source. Empty source-account is the XDR for "the transaction source".
fn sponsorship_source(quote: &Quote, sponsor: &str) -> Result<Option<xdr::MuxedAccount>> {
    if quote.tx_source() == sponsor {
        Ok(None)
    } else {
        Ok(Some(muxed_account(sponsor)?))
    }
}

fn path_assets(path: &[String]) -> Result<xdr::VecM<xdr::Asset, 5>> {
    let assets = path
        .iter()
        .map(|a| to_xdr_asset(&parse_asset(a)?))
        .collect::<Result<Vec<_>>>()?;
    assets
        .try_into()
        .map_err(|_| Error::Unsupported("path too long".into()))
}

fn op(source: Option<xdr::MuxedAccount>, body: xdr::OperationBody) -> xdr::Operation {
    xdr::Operation {
        source_account: source,
        body,
    }
}

/// Parse a 7-decimal amount string into stroops, rejecting anything else.
pub fn parse_amount(amount: &str) -> Result<i64> {
    reserve_horizon::parse_amount_stroops(amount).ok_or_else(|| Error::Amount(amount.to_string()))
}

/// Base64 XDR of a transaction, used for byte-for-byte comparison.
pub fn tx_to_xdr(tx: &xdr::Transaction) -> Result<String> {
    tx.to_xdr_base64(Limits::none())
        .map_err(|e| Error::Xdr(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quote::Quote;

    const USER: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";
    const SPONSOR: &str = "GBAW5XGWORWVFE2XTJYDTLDHXTY2Q2MO73HYCGB3XMFMQ562Q2W2GJQX";

    fn quote(mode: Mode, ops: Vec<UserOp>, reserve_stroops: i64) -> Quote {
        Quote {
            network: "Test SDF Network ; September 2015".into(),
            mode,
            source: USER.into(),
            sponsor: SPONSOR.into(),
            channel: String::new(),
            ops,
            fee_token: format!("USDC:{SPONSOR}"),
            charge_stroops: 360,
            send_max_stroops: 303,
            path: vec![],
            reserve_stroops,
            sequence: 43,
            inner_fee_stroops: 400,
            min_time: 0,
            max_time: 1_800_000_000,
            expires_at_ledger: 120,
        }
    }

    #[test]
    fn sponsored_trustline_is_a_sandwich_plus_a_fee_payment() {
        let q = quote(
            Mode::Sponsored,
            vec![UserOp::ChangeTrust {
                asset: format!("USDC:{SPONSOR}"),
                limit: None,
            }],
            5_000_000,
        );
        let tx = build_inner(&q).unwrap();
        assert_eq!(tx.operations.len(), 4);
        assert!(matches!(
            tx.operations[0].body,
            xdr::OperationBody::BeginSponsoringFutureReserves(_)
        ));
        assert_eq!(
            tx.operations[0].source_account,
            Some(muxed_account(SPONSOR).unwrap())
        );
        assert!(matches!(
            tx.operations[1].body,
            xdr::OperationBody::ChangeTrust(_)
        ));
        assert!(matches!(
            tx.operations[2].body,
            xdr::OperationBody::EndSponsoringFutureReserves
        ));
        // The user sources the transaction and pays us in their token.
        assert_eq!(tx.source_account, muxed_account(USER).unwrap());
        match &tx.operations[3].body {
            xdr::OperationBody::PathPaymentStrictReceive(p) => {
                assert_eq!(p.dest_asset, xdr::Asset::Native);
                assert_eq!(p.dest_amount, 360);
                assert_eq!(p.send_max, 303);
                assert_eq!(p.destination, muxed_account(SPONSOR).unwrap());
            }
            other => panic!("expected a path payment, got {other:?}"),
        }
        // The end of the sandwich must be signed by the sponsored account.
        assert_eq!(
            tx.operations[2].source_account,
            Some(muxed_account(USER).unwrap())
        );
    }

    const BALANCE_ID: &str =
        "00000000da0d57da7d4850e7fc10d2a9d0ebc731f7afb40574c03395b17d49149b91f5be";

    #[test]
    fn bootstrap_claims_its_funding_and_pays_out_of_it() {
        let q = quote(
            Mode::Bootstrap,
            vec![
                UserOp::CreateAccount {
                    destination: USER.into(),
                },
                UserOp::ClaimBalance {
                    balance_id: BALANCE_ID.into(),
                },
            ],
            10_000_000,
        );
        let tx = build_inner(&q).unwrap();
        assert_eq!(tx.source_account, muxed_account(SPONSOR).unwrap());
        // begin, create, claim, end, fee payment.
        assert_eq!(tx.operations.len(), 5);
        match &tx.operations[1].body {
            xdr::OperationBody::CreateAccount(c) => assert_eq!(c.starting_balance, 0),
            other => panic!("expected createAccount, got {other:?}"),
        }
        // createAccount is funded by the source; nothing else may default to it.
        assert_eq!(tx.operations[1].source_account, None);
        assert!(matches!(
            tx.operations[2].body,
            xdr::OperationBody::ClaimClaimableBalance(_)
        ));
        // The new account claims, and pays us out of what it just claimed.
        assert_eq!(
            tx.operations[2].source_account,
            Some(muxed_account(USER).unwrap())
        );
        match &tx.operations[4].body {
            xdr::OperationBody::PathPaymentStrictReceive(p) => {
                assert_eq!(p.dest_amount, 360);
                assert_eq!(p.destination, muxed_account(SPONSOR).unwrap());
            }
            other => panic!("expected the fee payment, got {other:?}"),
        }
        assert_eq!(
            tx.operations[4].source_account,
            Some(muxed_account(USER).unwrap())
        );
    }

    #[test]
    fn balance_ids_are_checked() {
        assert!(parse_balance_id(BALANCE_ID).is_ok());
        assert!(parse_balance_id("deadbeef").is_err());
        // Wrong type prefix.
        assert!(parse_balance_id(&format!("00000001{}", &BALANCE_ID[8..])).is_err());
    }

    #[test]
    fn building_is_deterministic() {
        let q = quote(
            Mode::Sponsored,
            vec![UserOp::Payment {
                destination: SPONSOR.into(),
                asset: format!("USDC:{SPONSOR}"),
                amount: "1.5".into(),
            }],
            0,
        );
        assert_eq!(
            tx_to_xdr(&build_inner(&q).unwrap()).unwrap(),
            tx_to_xdr(&build_inner(&q).unwrap()).unwrap()
        );
    }

    #[test]
    fn a_strict_send_swap_is_in_the_inner_transaction() {
        let usdc = format!("USDC:{SPONSOR}");
        let q = quote(
            Mode::Sponsored,
            vec![UserOp::PathPaymentStrictSend {
                destination: USER.into(),
                send_asset: usdc,
                send_amount: "1".into(),
                dest_asset: "native".into(),
                dest_min: "5".into(),
                path: vec![],
            }],
            0,
        );
        let tx = build_inner(&q).unwrap();
        let swap = tx
            .operations
            .iter()
            .find(|o| matches!(o.body, xdr::OperationBody::PathPaymentStrictSend(_)))
            .expect("swap op");
        match &swap.body {
            xdr::OperationBody::PathPaymentStrictSend(op) => {
                assert_eq!(op.send_amount, 10_000_000);
                assert_eq!(op.dest_min, 50_000_000);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn bootstrap_trustlines_belong_to_the_user_not_the_sponsor() {
        let q = quote(
            Mode::Bootstrap,
            vec![
                UserOp::CreateAccount {
                    destination: USER.into(),
                },
                UserOp::ChangeTrust {
                    asset: format!("USDC:{SPONSOR}"),
                    limit: None,
                },
                UserOp::ClaimBalance {
                    balance_id: BALANCE_ID.into(),
                },
            ],
            15_000_000,
        );
        let tx = build_inner(&q).unwrap();
        let trust = tx
            .operations
            .iter()
            .find(|o| matches!(o.body, xdr::OperationBody::ChangeTrust(_)))
            .unwrap();
        assert_eq!(trust.source_account, Some(muxed_account(USER).unwrap()));
    }

    #[test]
    fn a_channel_sources_bootstrap_and_the_sponsor_still_opens_the_sandwich() {
        let mut q = quote(
            Mode::Bootstrap,
            vec![
                UserOp::CreateAccount {
                    destination: USER.into(),
                },
                UserOp::ClaimBalance {
                    balance_id: BALANCE_ID.into(),
                },
            ],
            10_000_000,
        );
        q.channel = USER.to_string(); // any other G…; the user key is a stand-in
        let tx = build_inner(&q).unwrap();
        assert_eq!(tx.source_account, muxed_account(USER).unwrap());
        assert_eq!(
            tx.operations[0].source_account,
            Some(muxed_account(SPONSOR).unwrap())
        );
        match &tx.operations[0].body {
            xdr::OperationBody::BeginSponsoringFutureReserves(b) => {
                assert_eq!(b.sponsored_id, account_id(USER).unwrap());
            }
            other => panic!("expected beginSponsoring, got {other:?}"),
        }
    }

    #[test]
    fn unspecified_trustline_limit_is_the_maximum() {
        let q = quote(
            Mode::Sponsored,
            vec![UserOp::ChangeTrust {
                asset: format!("USDC:{SPONSOR}"),
                limit: None,
            }],
            5_000_000,
        );
        let tx = build_inner(&q).unwrap();
        match &tx.operations[1].body {
            xdr::OperationBody::ChangeTrust(c) => assert_eq!(c.limit, i64::MAX),
            other => panic!("expected changeTrust, got {other:?}"),
        }
    }
}
