use reserve_core::pricing::NetworkParams;
use reserve_horizon::{Horizon, HorizonApi};

use crate::channels::ChannelPool;
use crate::config::Config;
use crate::limits::Limits;
use crate::metrics::Metrics;

/// The number an operator has to watch: if `spendable_stroops` reaches zero,
/// every transaction fails at once.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SponsorFunds {
    pub balance_stroops: i64,
    pub locked_stroops: i64,
    pub spendable_stroops: i64,
    pub sponsoring: u32,
}

pub struct AppState {
    pub config: Config,
    pub horizon: Horizon,
    pub limits: Limits,
    pub metrics: Metrics,
    pub channels: ChannelPool,
}

impl AppState {
    pub fn new(config: Config) -> anyhow::Result<AppState> {
        let horizon = Horizon::new(&config.horizon_url);
        let limits = Limits::new(config.limits.clone());
        let channel_keys = if config.channel_secrets.is_empty() {
            vec![config.sponsor.clone()]
        } else {
            config.channel_secrets.clone()
        };
        let channels = ChannelPool::new(channel_keys);
        Ok(AppState {
            config,
            horizon,
            limits,
            metrics: Metrics::default(),
            channels,
        })
    }

    /// What the sponsor has, and what of it is immobilised in the reserves it
    /// carries for other people.
    ///
    /// Read from the chain rather than from a ledger of our own: the account's
    /// own `num_sponsoring` is the authority on what it sponsors, and a copy
    /// could only ever be wrong.
    pub async fn sponsor_funds(&self) -> reserve_horizon::Result<SponsorFunds> {
        let address = self.config.sponsor.address();
        let account = self
            .horizon
            .account(&address)
            .await?
            .ok_or_else(|| reserve_horizon::Error::NotFound(address))?;
        let base_reserve = self.horizon.latest_ledger().await?.base_reserve_in_stroops;
        let balance = account.native_balance_stroops().unwrap_or(0);
        // Two reserves for the account itself, plus one per entry it sponsors
        // for somebody else.
        let locked = (2 + account.num_sponsoring as i64) * base_reserve;
        Ok(SponsorFunds {
            balance_stroops: balance,
            locked_stroops: locked,
            spendable_stroops: balance - locked,
            sponsoring: account.num_sponsoring,
        })
    }

    /// Network parameters are read from the latest ledger rather than
    /// hardcoded: base fee and base reserve are protocol-upgradeable values.
    pub async fn network_params(&self) -> reserve_horizon::Result<NetworkParams> {
        let ledger = self.horizon.latest_ledger().await?;
        Ok(NetworkParams {
            base_fee_stroops: ledger.base_fee_in_stroops,
            base_reserve_stroops: ledger.base_reserve_in_stroops,
            latest_ledger: ledger.sequence,
        })
    }
}
