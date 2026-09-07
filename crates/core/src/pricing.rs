//! Turning a plan into a price the user pays in their own token.
//!
//! There is no oracle here on purpose: the exchange rate is whatever the SDEX
//! and liquidity pools will actually give us, read through Horizon's
//! strict-receive path finding, which is the same route the payment will take.

use reserve_horizon::{Asset, HorizonApi, PathRecord};

use crate::plan::Plan;
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct NetworkParams {
    pub base_fee_stroops: i64,
    pub base_reserve_stroops: i64,
    pub latest_ledger: u32,
}

#[derive(Debug, Clone)]
pub struct PricingConfig {
    /// Our margin over the raw on-chain cost, in basis points.
    pub margin_bps: u32,
    /// Fallback head-room over the quoted send amount, in basis points, used
    /// for assets the curated registry says nothing about. Listed assets carry
    /// their own measured value — see [`crate::tokens::slippage_bps`].
    pub slippage_bps: u32,
    /// How long a quote stays valid, in ledgers (~5s each).
    pub validity_ledgers: u32,
    /// Floor on what a sponsored transaction costs, in stroops of XLM.
    ///
    /// A percentage of Stellar's network fee is not a price: the whole fee for
    /// a payment is about 0.000006 XLM, and a margin on that rounds to nothing.
    /// What the user is paying for is not touching XLM at all, so the charge
    /// has a floor that does not depend on how cheap the network happens to be.
    pub min_charge_stroops: i64,
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            margin_bps: 2_000,
            slippage_bps: 100,
            validity_ledgers: 12,
            // ~$0.001 at XLM around $0.18. Denominated in stroops rather than
            // dollars on purpose: no price feed, no surprise. Revisit if XLM
            // moves a long way.
            min_charge_stroops: 56_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Cost {
    /// What the network actually costs us in XLM: fees plus locked reserves.
    pub network_fee_stroops: i64,
    pub reserve_stroops: i64,
    /// What we ask the user for, in XLM terms, margin included.
    pub charge_stroops: i64,
    /// The same amount in the user's token, plus slippage room.
    pub send_max_stroops: i64,
    /// The head-room actually applied, so callers can show the worst case.
    pub slippage_bps: u32,
    /// The route the path payment will take.
    pub path: Vec<Asset>,
}

/// Add `bps` basis points, rounding up.
///
/// Rounding down would quietly erase the margin and the slippage band at the
/// sizes this service actually charges: 1% of 65 stroops truncates to zero.
fn apply_bps(value: i64, bps: u32) -> i64 {
    let extra = (value as i128 * bps as i128 + 9_999) / 10_000;
    value.saturating_add(extra as i64)
}

/// Price a plan. In bootstrap mode there is nothing to charge against, so the
/// cost is computed but the caller is expected to subsidise it.
pub async fn price<H: HorizonApi>(
    horizon: &H,
    plan: &Plan,
    fee_token: &Asset,
    source_account: &str,
    params: &NetworkParams,
    cfg: &PricingConfig,
    network_passphrase: &str,
) -> Result<Cost> {
    // How far the price may move before the payment stops being acceptable is
    // a property of the asset, not of the deployment.
    let slippage_bps = crate::tokens::slippage_bps(fee_token, network_passphrase, cfg.slippage_bps);
    // The fee-bump adds one operation on top of the inner ones.
    let op_count = plan.op_count() as i64 + 1;
    let network_fee_stroops = params.base_fee_stroops.saturating_mul(op_count);
    let reserve_stroops = plan.reserve_stroops(params.base_reserve_stroops);
    let charge_stroops = apply_bps(
        network_fee_stroops.saturating_add(reserve_stroops),
        cfg.margin_bps,
    )
    // Reserves are real money we hand over, so they are never discounted; the
    // floor only ever raises a charge that would otherwise be dust.
    .max(cfg.min_charge_stroops.saturating_add(reserve_stroops));

    if matches!(fee_token, Asset::Native) {
        // Paying in XLM needs no conversion; the payment operation still runs
        // so the accounting stays uniform.
        return Ok(Cost {
            network_fee_stroops,
            reserve_stroops,
            charge_stroops,
            // XLM for XLM: there is nothing to slip.
            send_max_stroops: apply_bps(charge_stroops, slippage_bps),
            slippage_bps,
            path: Vec::new(),
        });
    }

    let (source_amount, path) =
        token_amount_for_xlm(horizon, fee_token, source_account, charge_stroops).await?;

    Ok(Cost {
        network_fee_stroops,
        reserve_stroops,
        charge_stroops,
        send_max_stroops: apply_bps(source_amount, slippage_bps),
        slippage_bps,
        path,
    })
}

/// Amounts below this are too small for path finding to price sensibly, so the
/// rate is always taken at this size and scaled down. One lumen.
const REFERENCE_STROOPS: i64 = 10_000_000;

/// How much of `token` buys `xlm_stroops`, and by which route.
///
/// The rate is always read at a meaningful size and then scaled, never asked
/// for directly at the size we need. Fees here are dust — a classic fee is
/// around 0.000036 XLM — and at that size Horizon returns rounding artefacts:
/// asking what buys 0.0000360 XLM offers routes costing 0.0000001 USDC,
/// roughly a sixtieth of the real price. Quoting from those would set a
/// `sendMax` the payment can never satisfy, and every such transaction would
/// fail with the fee bump charged to us.
///
/// Scaling from a larger size errs the other way — it carries that size's price
/// impact — which is the safe direction: `sendMax` is a ceiling, and
/// strict-receive still spends only what the market asks.
pub async fn token_amount_for_xlm<H: HorizonApi>(
    horizon: &H,
    token: &Asset,
    source_account: &str,
    xlm_stroops: i64,
) -> Result<(i64, Vec<Asset>)> {
    let reference = xlm_stroops.max(REFERENCE_STROOPS);
    let records = horizon
        .strict_receive_paths(
            source_account,
            std::slice::from_ref(token),
            &Asset::Native,
            reference,
        )
        .await?;
    let best = cheapest(&records).ok_or_else(|| Error::NoPath {
        token: token.canonical(),
    })?;
    let reference_cost = best
        .source_amount_stroops()
        .ok_or_else(|| Error::Amount(best.source_amount.clone()))?;
    if reference_cost <= 0 {
        return Err(Error::NoPath {
            token: token.canonical(),
        });
    }
    let path = best
        .path
        .iter()
        .map(|hop| {
            hop.to_asset()
                .ok_or_else(|| Error::Asset("path hop".into()))
        })
        .collect::<Result<Vec<_>>>()?;

    // Scale to the amount actually being bought, rounding up: paying a stroop
    // too much is free, paying one too little fails the transaction.
    let scaled =
        (xlm_stroops as i128 * reference_cost as i128 + reference as i128 - 1) / reference as i128;
    Ok((scaled.max(1) as i64, path))
}

fn cheapest(records: &[PathRecord]) -> Option<&PathRecord> {
    records
        .iter()
        .filter(|r| r.source_amount_stroops().is_some())
        .min_by_key(|r| r.source_amount_stroops().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::UserOp;
    use crate::tokens::{PUBLIC_NETWORK, TESTNET};
    use reserve_horizon::{Account, FeeStats, Ledger, PathHop, SubmitResponse};

    const G: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";

    struct FakeHorizon {
        records: Vec<PathRecord>,
    }

    impl HorizonApi for FakeHorizon {
        async fn account(&self, _id: &str) -> reserve_horizon::Result<Option<Account>> {
            Ok(None)
        }
        async fn strict_receive_paths(
            &self,
            _source_account: &str,
            _source_assets: &[Asset],
            _destination: &Asset,
            _destination_amount_stroops: i64,
        ) -> reserve_horizon::Result<Vec<PathRecord>> {
            Ok(self.records.clone())
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
        ) -> reserve_horizon::Result<Option<reserve_horizon::TransactionRecord>> {
            unimplemented!()
        }
        async fn claimable_balance(
            &self,
            _id: &str,
        ) -> reserve_horizon::Result<Option<reserve_horizon::ClaimableBalance>> {
            Ok(None)
        }
    }

    /// A route whose price is quoted per reference lumen.
    fn record(source_amount: &str) -> PathRecord {
        PathRecord {
            source_amount: source_amount.into(),
            destination_amount: "1.0000000".into(),
            source_asset_type: "credit_alphanum4".into(),
            source_asset_code: Some("USDC".into()),
            source_asset_issuer: Some(G.into()),
            path: vec![PathHop {
                asset_type: "native".into(),
                asset_code: None,
                asset_issuer: None,
            }],
        }
    }

    fn params() -> NetworkParams {
        NetworkParams {
            base_fee_stroops: 100,
            base_reserve_stroops: 5_000_000,
            latest_ledger: 100,
        }
    }

    #[tokio::test]
    async fn picks_the_cheapest_route_and_adds_slippage() {
        // 0.1795 and 0.5 USDC per lumen; the cheaper route wins.
        let horizon = FakeHorizon {
            records: vec![record("0.5000000"), record("0.1795000")],
        };
        let plan = Plan::new(
            vec![UserOp::Payment {
                destination: G.into(),
                asset: format!("USDC:{G}"),
                amount: "1".into(),
            }],
            true,
        )
        .unwrap();
        let token = Asset::parse(&format!("USDC:{G}")).unwrap();
        let cfg = PricingConfig {
            min_charge_stroops: 0,
            ..PricingConfig::default()
        };
        let cost = super::price(&horizon, &plan, &token, G, &params(), &cfg, TESTNET)
            .await
            .unwrap();

        // 2 inner ops + 1 for the fee bump, at 100 stroops each.
        assert_eq!(cost.network_fee_stroops, 300);
        assert_eq!(cost.reserve_stroops, 0);
        // 300 stroops + 20% margin.
        assert_eq!(cost.charge_stroops, 360);
        // 360 stroops of XLM at 0.1795 USDC/XLM is 64.6 stroops of USDC, which
        // rounds up to 65, plus 1% slippage rounded up.
        assert_eq!(cost.send_max_stroops, 66);
    }

    #[tokio::test]
    async fn a_listed_asset_uses_its_own_slippage() {
        let horizon = FakeHorizon {
            records: vec![record("0.1795000")],
        };
        let plan = Plan::new(
            vec![UserOp::Payment {
                destination: G.into(),
                asset: format!("USDC:{G}"),
                amount: "1".into(),
            }],
            true,
        )
        .unwrap();
        let usdc =
            Asset::parse("USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN").unwrap();
        // The deployment default is deliberately absurd; the registry wins.
        let cfg = PricingConfig {
            slippage_bps: 9_000,
            min_charge_stroops: 0,
            ..PricingConfig::default()
        };
        let cost = super::price(&horizon, &plan, &usdc, G, &params(), &cfg, PUBLIC_NETWORK)
            .await
            .unwrap();
        assert_eq!(cost.slippage_bps, 100);
        assert_eq!(cost.send_max_stroops, 66);
    }

    #[tokio::test]
    async fn paying_in_xlm_has_no_slippage_at_all() {
        let horizon = FakeHorizon { records: vec![] };
        let plan = Plan::new(
            vec![UserOp::Payment {
                destination: G.into(),
                asset: "native".into(),
                amount: "1".into(),
            }],
            true,
        )
        .unwrap();
        let cfg = PricingConfig {
            slippage_bps: 500,
            min_charge_stroops: 0,
            ..PricingConfig::default()
        };
        let cost = super::price(
            &horizon,
            &plan,
            &Asset::Native,
            G,
            &params(),
            &cfg,
            PUBLIC_NETWORK,
        )
        .await
        .unwrap();
        assert_eq!(cost.slippage_bps, 0);
        assert_eq!(cost.send_max_stroops, cost.charge_stroops);
    }

    #[tokio::test]
    async fn dust_sized_rounding_artefacts_do_not_set_the_price() {
        // What mainnet actually returns when asked to price a dust amount
        // directly: routes costing a fraction of the real rate. Pricing has to
        // read the rate at a usable size and scale it down instead.
        let horizon = FakeHorizon {
            records: vec![record("0.1789103"), record("0.1789179")],
        };
        let plan = Plan::new(
            vec![UserOp::Payment {
                destination: G.into(),
                asset: format!("USDC:{G}"),
                amount: "1".into(),
            }],
            true,
        )
        .unwrap();
        let token = Asset::parse(&format!("USDC:{G}")).unwrap();
        let cfg = PricingConfig {
            min_charge_stroops: 0,
            ..PricingConfig::default()
        };
        let cost = super::price(&horizon, &plan, &token, G, &params(), &cfg, TESTNET)
            .await
            .unwrap();
        // 360 stroops of XLM is worth ~64 stroops of USDC — not the single
        // stroop a naive dust quote would have produced.
        assert!(cost.send_max_stroops >= 64, "got {}", cost.send_max_stroops);
        assert!(cost.send_max_stroops <= 70, "got {}", cost.send_max_stroops);
    }

    #[tokio::test]
    async fn no_liquidity_is_rejected_at_quote_time() {
        let horizon = FakeHorizon { records: vec![] };
        let plan = Plan::new(
            vec![UserOp::Payment {
                destination: G.into(),
                asset: format!("USDC:{G}"),
                amount: "1".into(),
            }],
            true,
        )
        .unwrap();
        let token = Asset::parse(&format!("USDC:{G}")).unwrap();
        let err = super::price(
            &horizon,
            &plan,
            &token,
            G,
            &params(),
            &PricingConfig::default(),
            TESTNET,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::NoPath { .. }));
    }

    #[tokio::test]
    async fn a_new_account_pays_for_its_own_reserves() {
        let horizon = FakeHorizon { records: vec![] };
        let plan =
            Plan::new(
                vec![
                UserOp::CreateAccount { destination: G.into() },
                UserOp::ClaimBalance {
                    balance_id:
                        "00000000da0d57da7d4850e7fc10d2a9d0ebc731f7afb40574c03395b17d49149b91f5be"
                            .into(),
                },
            ],
                false,
            )
            .unwrap();
        let cost = super::price(
            &horizon,
            &plan,
            &Asset::Native,
            G,
            &params(),
            &PricingConfig::default(),
            TESTNET,
        )
        .await
        .unwrap();
        // The 1 XLM of reserves is passed on in full, never discounted.
        assert_eq!(cost.reserve_stroops, 10_000_000);
        assert!(cost.charge_stroops > cost.reserve_stroops);
        assert_eq!(cost.send_max_stroops, cost.charge_stroops);
    }

    #[tokio::test]
    async fn the_floor_applies_when_the_network_fee_is_dust() {
        let horizon = FakeHorizon {
            records: vec![record("0.1795000")],
        };
        let plan = Plan::new(
            vec![UserOp::Payment {
                destination: G.into(),
                asset: format!("USDC:{G}"),
                amount: "1".into(),
            }],
            true,
        )
        .unwrap();
        let token = Asset::parse(&format!("USDC:{G}")).unwrap();
        let cost = super::price(
            &horizon,
            &plan,
            &token,
            G,
            &params(),
            &PricingConfig::default(),
            TESTNET,
        )
        .await
        .unwrap();
        // 20% over 300 stroops would be 360; the floor is what actually applies.
        assert_eq!(cost.charge_stroops, 56_000);
    }
}
