//! Last look at the quoted fee route before we put a fee-bump on chain.
//!
//! The inner transaction freezes Horizon's hops. If that route dies or now
//! costs more than `sendMax`, submitting still spends the sponsor's fee-bump.
//! This check is the same shape as `claim::ensure_claims`: Horizon is not the
//! ledger, so a path can still move between this GET and apply. Refusing here
//! is how we avoid paying for a transaction we already know cannot collect.

use reserve_horizon::{Asset, HorizonApi, PathRecord};

use crate::{Error, Result};

/// True when `record` is the route the quote locked in.
pub fn record_matches_quoted_path(record: &PathRecord, quoted: &[String]) -> bool {
    let hops: Vec<String> = record
        .path
        .iter()
        .filter_map(|hop| hop.to_asset().map(|asset| asset.canonical()))
        .collect();
    if hops.len() != quoted.len() {
        return false;
    }
    hops.iter().zip(quoted).all(|(got, want)| same_canonical(got, want))
}

fn same_canonical(a: &str, b: &str) -> bool {
    match (Asset::parse(a), Asset::parse(b)) {
        (Some(left), Some(right)) => left == right,
        _ => a == b,
    }
}

/// Refuse to fee-bump when the quoted hops cannot still buy `charge_stroops`
/// of XLM without breaking `send_max_stroops`.
///
/// Native payments skip this: nothing is converted. A missing matching hop
/// list is a miss even if some other route is cheaper — that cheaper route
/// is not what the signed XDR will execute.
pub async fn ensure_quoted_path<H: HorizonApi>(
    horizon: &H,
    source: &str,
    fee_token: &str,
    charge_stroops: i64,
    send_max_stroops: i64,
    quoted_path: &[String],
) -> Result<()> {
    let token = Asset::parse(fee_token).ok_or_else(|| Error::Asset(fee_token.into()))?;
    if matches!(token, Asset::Native) {
        return Ok(());
    }
    if charge_stroops <= 0 {
        return Ok(());
    }

    let records = horizon
        .strict_receive_paths(
            source,
            std::slice::from_ref(&token),
            &Asset::Native,
            charge_stroops,
        )
        .await?;

    let Some(matched) = records
        .iter()
        .find(|record| record_matches_quoted_path(record, quoted_path))
    else {
        return Err(Error::PathMoved(format!(
            "the quoted {}→XLM hops are gone; re-quote",
            token.canonical()
        )));
    };

    let need = matched
        .source_amount_stroops()
        .ok_or_else(|| Error::Amount(matched.source_amount.clone()))?;
    if need > send_max_stroops {
        return Err(Error::PathMoved(format!(
            "the quoted route now costs {need} stroops, sendMax is {send_max_stroops}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reserve_horizon::{
        Account, Asset, ClaimableBalance, FeeStats, Ledger, PathHop, PathRecord, SubmitResponse,
        TransactionRecord,
    };

    const USER: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";
    const ISSUER: &str = "GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN";
    const USDC: &str = "USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN";

    struct FakeHorizon {
        records: Vec<PathRecord>,
    }

    impl HorizonApi for FakeHorizon {
        async fn account(&self, _: &str) -> reserve_horizon::Result<Option<Account>> {
            unimplemented!()
        }
        async fn strict_receive_paths(
            &self,
            _: &str,
            _: &[Asset],
            _: &Asset,
            _: i64,
        ) -> reserve_horizon::Result<Vec<PathRecord>> {
            Ok(self.records.clone())
        }
        async fn fee_stats(&self) -> reserve_horizon::Result<FeeStats> {
            unimplemented!()
        }
        async fn latest_ledger(&self) -> reserve_horizon::Result<Ledger> {
            unimplemented!()
        }
        async fn submit(&self, _: &str) -> reserve_horizon::Result<SubmitResponse> {
            unimplemented!()
        }
        async fn transaction(
            &self,
            _: &str,
        ) -> reserve_horizon::Result<Option<TransactionRecord>> {
            unimplemented!()
        }
        async fn claimable_balance(
            &self,
            _: &str,
        ) -> reserve_horizon::Result<Option<ClaimableBalance>> {
            unimplemented!()
        }
    }

    fn direct(source_amount: &str) -> PathRecord {
        PathRecord {
            source_amount: source_amount.into(),
            destination_amount: "0.0056000".into(),
            source_asset_type: "credit_alphanum4".into(),
            source_asset_code: Some("USDC".into()),
            source_asset_issuer: Some(ISSUER.into()),
            path: vec![],
        }
    }

    fn via_native(source_amount: &str) -> PathRecord {
        PathRecord {
            source_amount: source_amount.into(),
            destination_amount: "0.0056000".into(),
            source_asset_type: "credit_alphanum4".into(),
            source_asset_code: Some("USDC".into()),
            source_asset_issuer: Some(ISSUER.into()),
            path: vec![PathHop {
                asset_type: "native".into(),
                asset_code: None,
                asset_issuer: None,
            }],
        }
    }

    #[test]
    fn empty_quoted_path_is_the_direct_book() {
        assert!(record_matches_quoted_path(&direct("0.0010000"), &[]));
        assert!(!record_matches_quoted_path(&via_native("0.0010000"), &[]));
        assert!(record_matches_quoted_path(
            &via_native("0.0010000"),
            &["native".into()]
        ));
    }

    #[tokio::test]
    async fn native_fees_are_not_repriced() {
        let horizon = FakeHorizon { records: vec![] };
        ensure_quoted_path(&horizon, USER, "native", 56_000, 56_000, &[])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_live_direct_route_inside_send_max_passes() {
        let horizon = FakeHorizon {
            records: vec![direct("0.0010000")],
        };
        ensure_quoted_path(&horizon, USER, USDC, 56_000, 10_100, &[])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_route_past_send_max_is_refused() {
        let horizon = FakeHorizon {
            records: vec![direct("0.0020000")],
        };
        let err = ensure_quoted_path(&horizon, USER, USDC, 56_000, 10_100, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::PathMoved(m) if m.contains("sendMax")));
    }

    #[tokio::test]
    async fn a_cheaper_but_different_hop_list_is_not_the_quoted_route() {
        let horizon = FakeHorizon {
            records: vec![via_native("0.0009000")],
        };
        let err = ensure_quoted_path(&horizon, USER, USDC, 56_000, 10_100, &[])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::PathMoved(m) if m.contains("hops are gone")));
    }
}
