//! Whether a `claim_balance` will actually succeed.
//!
//! The plan only checks that the id is 72 hex characters. That is enough to
//! build a well-formed operation, and not enough to keep the sponsor's
//! sequence number: Horizon will accept the transaction, consume the sequence,
//! and then fail the claim. The probe that showed this used a random id and
//! an unfunded attacker.
//!
//! Checking at quote time stops us handing out a transaction that cannot work.
//! Checking again at submit time is what saves the sequence — a balance can
//! vanish or vest between the two.

use reserve_horizon::HorizonApi;

use crate::plan::{Mode, UserOp};
use crate::{Error, Result};

/// What the arriving funds have to cover, when the account has nothing else.
pub struct ClaimNeed {
    pub asset: String,
    pub min_stroops: i64,
}

/// Refuse any `claim_balance` that Horizon says is missing, not yet
/// claimable by `source`, or — in bootstrap — too small to pay the fee.
pub async fn ensure_claims<H: HorizonApi>(
    horizon: &H,
    source: &str,
    ops: &[UserOp],
    mode: Mode,
    now: i64,
    need: Option<&ClaimNeed>,
) -> Result<()> {
    let mut payable = 0i64;
    let mut saw_fee_asset = false;
    let mut saw_claim = false;

    for op in ops {
        let UserOp::ClaimBalance { balance_id } = op else {
            continue;
        };
        saw_claim = true;
        let Some(balance) = horizon.claimable_balance(balance_id).await? else {
            return Err(Error::Unclaimable(format!(
                "claimable balance {balance_id} does not exist"
            )));
        };
        if !balance.claimable_by(source, now) {
            return Err(Error::Unclaimable(format!(
                "{source} cannot claim {balance_id} yet"
            )));
        }
        if let Some(need) = need {
            if balance.asset == need.asset {
                saw_fee_asset = true;
                let amount = balance.amount_stroops().ok_or_else(|| {
                    Error::Unclaimable("claimable balance amount is unreadable".into())
                })?;
                payable = payable.saturating_add(amount);
            }
        }
    }

    if let Some(need) = need {
        if mode == Mode::Bootstrap && saw_claim && !saw_fee_asset {
            return Err(Error::Unclaimable(format!(
                "the arriving funds are not in {}, so the new account cannot pay the fee",
                need.asset
            )));
        }
        if saw_fee_asset && payable < need.min_stroops {
            return Err(Error::Unclaimable(format!(
                "claimable funds are {payable} stroops, the fee may take {}",
                need.min_stroops
            )));
        }
    }
    Ok(())
}

/// Bootstrap has to pay for itself out of what it claims; sponsored claims
/// only have to exist and be claimable.
pub fn need_for(mode: Mode, fee_token: &str, send_max_stroops: i64) -> Option<ClaimNeed> {
    (mode == Mode::Bootstrap).then(|| ClaimNeed {
        asset: fee_token.to_string(),
        min_stroops: send_max_stroops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reserve_horizon::{
        Account, Asset, ClaimableBalance, Claimant, FeeStats, Ledger, PathRecord, Predicate,
        SubmitResponse, TransactionRecord,
    };

    const USER: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";
    const OTHER: &str = "GBAW5XGWORWVFE2XTJYDTLDHXTY2Q2MO73HYCGB3XMFMQ562Q2W2GJQX";
    const ID: &str = "00000000da0d57da7d4850e7fc10d2a9d0ebc731f7afb40574c03395b17d49149b91f5be";
    const USDC: &str = "USDC:GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";

    struct FakeHorizon {
        balance: Option<ClaimableBalance>,
    }

    impl HorizonApi for FakeHorizon {
        async fn account(&self, _id: &str) -> reserve_horizon::Result<Option<Account>> {
            unimplemented!()
        }
        async fn strict_receive_paths(
            &self,
            _a: &str,
            _b: &[Asset],
            _c: &Asset,
            _d: i64,
        ) -> reserve_horizon::Result<Vec<PathRecord>> {
            unimplemented!()
        }
        async fn fee_stats(&self) -> reserve_horizon::Result<FeeStats> {
            unimplemented!()
        }
        async fn latest_ledger(&self) -> reserve_horizon::Result<Ledger> {
            unimplemented!()
        }
        async fn submit(&self, _xdr: &str) -> reserve_horizon::Result<SubmitResponse> {
            unimplemented!()
        }
        async fn transaction(
            &self,
            _hash: &str,
        ) -> reserve_horizon::Result<Option<TransactionRecord>> {
            unimplemented!()
        }
        async fn claimable_balance(
            &self,
            _id: &str,
        ) -> reserve_horizon::Result<Option<ClaimableBalance>> {
            Ok(self.balance.clone())
        }
    }

    fn ops() -> Vec<UserOp> {
        vec![
            UserOp::CreateAccount {
                destination: USER.into(),
            },
            UserOp::ClaimBalance {
                balance_id: ID.into(),
            },
        ]
    }

    fn balance(over: impl FnOnce(&mut ClaimableBalance)) -> ClaimableBalance {
        let mut b = ClaimableBalance {
            id: ID.into(),
            asset: USDC.into(),
            amount: "25.0000000".into(),
            last_modified_time: Some("2020-01-01T00:00:00Z".into()),
            claimants: vec![Claimant {
                destination: USER.into(),
                predicate: Predicate {
                    unconditional: Some(true),
                    ..Predicate::default()
                },
            }],
        };
        over(&mut b);
        b
    }

    fn need() -> ClaimNeed {
        ClaimNeed {
            asset: USDC.into(),
            min_stroops: 50_000_000,
        }
    }

    #[tokio::test]
    async fn a_missing_balance_is_refused() {
        let err = ensure_claims(
            &FakeHorizon { balance: None },
            USER,
            &ops(),
            Mode::Bootstrap,
            1,
            Some(&need()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Unclaimable(m) if m.contains("does not exist")));
    }

    #[tokio::test]
    async fn somebody_elses_balance_is_refused() {
        let horizon = FakeHorizon {
            balance: Some(balance(|b| {
                b.claimants[0].destination = OTHER.into();
            })),
        };
        let err = ensure_claims(&horizon, USER, &ops(), Mode::Bootstrap, 1, Some(&need()))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Unclaimable(m) if m.contains("cannot claim")));
    }

    #[tokio::test]
    async fn a_vesting_predicate_is_refused_until_it_opens() {
        let horizon = FakeHorizon {
            balance: Some(balance(|b| {
                b.claimants[0].predicate = Predicate {
                    not: Some(Box::new(Predicate {
                        rel_before: Some("7200".into()),
                        ..Predicate::default()
                    })),
                    ..Predicate::default()
                };
            })),
        };
        let created = 1_577_836_800; // 2020-01-01T00:00:00Z
        let err = ensure_claims(
            &horizon,
            USER,
            &ops(),
            Mode::Bootstrap,
            created + 100,
            Some(&need()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Unclaimable(m) if m.contains("cannot claim")));
        assert!(ensure_claims(
            &horizon,
            USER,
            &ops(),
            Mode::Bootstrap,
            created + 7_200,
            Some(&need())
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn bootstrap_refuses_funds_in_the_wrong_asset() {
        let horizon = FakeHorizon {
            balance: Some(balance(|b| b.asset = "native".into())),
        };
        let err = ensure_claims(&horizon, USER, &ops(), Mode::Bootstrap, 1, Some(&need()))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Unclaimable(m) if m.contains("not in")));
    }

    #[tokio::test]
    async fn bootstrap_refuses_funds_that_cannot_cover_the_fee() {
        let horizon = FakeHorizon {
            balance: Some(balance(|b| b.amount = "0.1000000".into())),
        };
        let err = ensure_claims(&horizon, USER, &ops(), Mode::Bootstrap, 1, Some(&need()))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Unclaimable(m) if m.contains("fee may take")));
    }

    #[tokio::test]
    async fn an_honest_claim_passes() {
        let horizon = FakeHorizon {
            balance: Some(balance(|_| {})),
        };
        assert!(
            ensure_claims(&horizon, USER, &ops(), Mode::Bootstrap, 1, Some(&need()))
                .await
                .is_ok()
        );
    }
}
