//! What the caller asked for, and what that costs in reserves.
//!
//! The set of operations Reserve will sponsor is an allowlist *by
//! construction*: [`UserOp`] simply cannot express a `SetOptions` that adds a
//! signer or changes thresholds, so no transaction the service builds can hand
//! anyone control of the user's account.

use serde::{Deserialize, Serialize};

use crate::asset::{parse_asset, Asset};
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserOp {
    /// Create `destination` with a zero starting balance, reserves sponsored.
    CreateAccount { destination: String },
    /// Pay `amount` of `asset` to `destination`.
    Payment {
        destination: String,
        asset: String,
        amount: String,
    },
    /// Open a trustline, its reserve sponsored.
    ChangeTrust {
        asset: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<String>,
    },
    /// Claim money someone left for this account. This is how an account that
    /// does not exist yet gets funded: a claimable balance can be created for
    /// an address before there is an account behind it.
    ClaimBalance {
        /// Horizon's 72-character hex balance id.
        balance_id: String,
    },
}

/// How the inner transaction has to be shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The user's account already exists: it sources the transaction, signs it,
    /// pays us in its own token, and the sponsor fee-bumps.
    Sponsored,
    /// Cold start — the account does not exist yet, so it can neither source a
    /// transaction nor hold anything to pay with. The sponsor sources and pays;
    /// the new account co-signs `endSponsoringFutureReserves`.
    Bootstrap,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub mode: Mode,
    pub ops: Vec<UserOp>,
    /// New account entries created for the user (2 base reserves each).
    pub new_accounts: u32,
    /// New subentries created for the user (1 base reserve each).
    pub new_subentries: u32,
}

impl Plan {
    /// Classify a request. `account_exists` comes from Horizon.
    pub fn new(ops: Vec<UserOp>, account_exists: bool) -> Result<Plan> {
        if ops.is_empty() {
            return Err(Error::Unsupported("no operations".into()));
        }
        if ops.len() > 20 {
            return Err(Error::Unsupported("too many operations".into()));
        }
        let mut new_accounts = 0;
        let mut new_subentries = 0;
        let mut claims = 0;
        for op in &ops {
            match op {
                UserOp::CreateAccount { destination } => {
                    crate::asset::account_id(destination)?;
                    new_accounts += 1;
                }
                UserOp::ChangeTrust { asset, .. } => {
                    match parse_asset(asset)? {
                        Asset::Native => {
                            return Err(Error::Asset("cannot trust the native asset".into()))
                        }
                        Asset::Credit { .. } => {}
                    }
                    new_subentries += 1;
                }
                UserOp::Payment {
                    destination, asset, ..
                } => {
                    crate::asset::account_id(destination)?;
                    parse_asset(asset)?;
                }
                UserOp::ClaimBalance { balance_id } => {
                    crate::build::parse_balance_id(balance_id)?;
                    claims += 1;
                }
            }
        }
        let mode = if account_exists {
            Mode::Sponsored
        } else {
            Mode::Bootstrap
        };
        // An account that does not exist yet has nothing to pay with, so
        // creating one only makes sense in the same transaction that funds it.
        // Otherwise the reserves would be a gift to an address that may never
        // come back — and once given they cannot be taken back: releasing a
        // sponsored reserve requires the sponsored account to cover it, which
        // an empty account never can.
        if mode == Mode::Bootstrap && claims == 0 {
            return Err(Error::Unsupported(
                "a new account has to be funded in the same transaction: include a claim_balance \
                 operation for money left to this address"
                    .into(),
            ));
        }
        Ok(Plan {
            mode,
            ops,
            new_accounts,
            new_subentries,
        })
    }

    pub fn needs_sponsorship(&self) -> bool {
        self.new_accounts > 0 || self.new_subentries > 0
    }

    /// XLM the sponsor will have locked (not spent) if this goes through.
    pub fn reserve_stroops(&self, base_reserve_stroops: i64) -> i64 {
        (self.new_accounts as i64 * 2 + self.new_subentries as i64) * base_reserve_stroops
    }

    /// Operations in the built inner transaction: the sponsorship sandwich, the
    /// user's own operations, and the fee payment. Every mode pays.
    pub fn op_count(&self) -> u32 {
        let sandwich = if self.needs_sponsorship() { 2 } else { 0 };
        self.ops.len() as u32 + sandwich + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "GDRXE2BQUC3AZNPVFSCEZ76NJ3WWL25FYFK6RGZGIEKWE4SOOHSUJUJ6";

    fn usdc() -> String {
        format!("USDC:{G}")
    }

    const BALANCE_ID: &str =
        "00000000da0d57da7d4850e7fc10d2a9d0ebc731f7afb40574c03395b17d49149b91f5be";

    #[test]
    fn reserves_follow_the_min_balance_formula() {
        let plan = Plan::new(
            vec![
                UserOp::CreateAccount {
                    destination: G.into(),
                },
                UserOp::ChangeTrust {
                    asset: usdc(),
                    limit: None,
                },
                UserOp::ClaimBalance {
                    balance_id: BALANCE_ID.into(),
                },
            ],
            false,
        )
        .unwrap();
        // 2 base reserves for the account + 1 for the trustline = 1.5 XLM.
        assert_eq!(plan.reserve_stroops(5_000_000), 15_000_000);
        assert_eq!(plan.mode, Mode::Bootstrap);
        // begin + create + trust + claim + end + fee payment.
        assert_eq!(plan.op_count(), 6);
    }

    #[test]
    fn a_new_account_has_to_arrive_with_money() {
        // Creating an empty account for free is how reserves get given away,
        // and Stellar offers no way to take them back.
        let err = Plan::new(
            vec![UserOp::CreateAccount {
                destination: G.into(),
            }],
            false,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));

        // With funds arriving in the same transaction it is fine.
        assert!(Plan::new(
            vec![
                UserOp::CreateAccount {
                    destination: G.into()
                },
                UserOp::ClaimBalance {
                    balance_id: BALANCE_ID.into()
                },
            ],
            false,
        )
        .is_ok());
    }

    #[test]
    fn sponsored_mode_adds_the_fee_operation() {
        let plan = Plan::new(
            vec![UserOp::Payment {
                destination: G.into(),
                asset: usdc(),
                amount: "1".into(),
            }],
            true,
        )
        .unwrap();
        assert!(!plan.needs_sponsorship());
        assert_eq!(plan.op_count(), 2);
        assert_eq!(plan.reserve_stroops(5_000_000), 0);
    }

    #[test]
    fn rejects_native_trustlines_and_empty_requests() {
        assert!(Plan::new(
            vec![UserOp::ChangeTrust {
                asset: "native".into(),
                limit: None
            }],
            true
        )
        .is_err());
        assert!(Plan::new(vec![], true).is_err());
    }
}
