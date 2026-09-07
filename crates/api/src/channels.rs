//! One in-flight bootstrap quote per lane.
//!
//! Stellar applies transactions from a source account in sequence order. Two
//! quotes that read the same account at the same time get the same number;
//! only the first submit can use it. A lane is leased at quote time and freed
//! at submit (or when the quote's time bound passes), so a second caller
//! either gets a different lane or is told to retry.
//!
//! With no extra keys configured the sponsor is the only lane: that is enough
//! to stop the collision. Extra `S…` secrets are extra lanes, and those
//! accounts source the transaction while the sponsor still opens the sandwich
//! (CAP-33) and pays the fee bump (CAP-15).

use std::time::{Duration, Instant};

use reserve_horizon::HorizonApi;
use reserve_signer::Sponsor;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("every bootstrap lane is busy")]
    Busy,
    #[error("channel account {0} does not exist")]
    Missing(String),
    #[error(transparent)]
    Horizon(#[from] reserve_horizon::Error),
}

struct Lease {
    sequence: i64,
    expires_at: Instant,
}

struct Lane {
    key: Sponsor,
    address: String,
    lease: Mutex<Option<Lease>>,
}

pub struct ChannelPool {
    lanes: Vec<Lane>,
}

impl ChannelPool {
    pub fn new(keys: Vec<Sponsor>) -> ChannelPool {
        let lanes = keys
            .into_iter()
            .map(|key| Lane {
                address: key.address(),
                key,
                lease: Mutex::new(None),
            })
            .collect();
        ChannelPool { lanes }
    }

    pub fn len(&self) -> usize {
        self.lanes.len()
    }

    pub fn signer(&self, address: &str) -> Option<&Sponsor> {
        self.lanes
            .iter()
            .find(|l| l.address == address)
            .map(|l| &l.key)
    }

    /// Reserve a lane and the next sequence number on it.
    ///
    /// The number is read from Horizon under the lane lock, so two concurrent
    /// quotes cannot be handed the same one. An expired lease is treated as
    /// free: the previous quote's `max_time` has passed, so the network will
    /// no longer accept it.
    pub async fn lease<H: HorizonApi>(
        &self,
        horizon: &H,
        ttl: Duration,
    ) -> Result<(String, i64), ChannelError> {
        let now = Instant::now();
        for lane in &self.lanes {
            let mut slot = lane.lease.lock().await;
            if slot.as_ref().is_some_and(|l| l.expires_at > now) {
                continue;
            }
            let account = horizon
                .account(&lane.address)
                .await?
                .ok_or_else(|| ChannelError::Missing(lane.address.clone()))?;
            let sequence = account
                .sequence_i64()
                .ok_or_else(|| ChannelError::Missing(lane.address.clone()))?
                + 1;
            *slot = Some(Lease {
                sequence,
                expires_at: now + ttl,
            });
            return Ok((lane.address.clone(), sequence));
        }
        Err(ChannelError::Busy)
    }

    pub async fn release(&self, address: &str, sequence: i64) {
        let Some(lane) = self.lanes.iter().find(|l| l.address == address) else {
            return;
        };
        let mut slot = lane.lease.lock().await;
        if slot.as_ref().is_some_and(|l| l.sequence == sequence) {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reserve_horizon::{
        Account, Asset, ClaimableBalance, FeeStats, Ledger, PathRecord, SubmitResponse,
        TransactionRecord,
    };

    struct FakeHorizon {
        sequence: std::sync::atomic::AtomicI64,
    }

    impl HorizonApi for FakeHorizon {
        async fn account(&self, _id: &str) -> reserve_horizon::Result<Option<Account>> {
            Ok(Some(Account {
                id: String::new(),
                sequence: self
                    .sequence
                    .load(std::sync::atomic::Ordering::SeqCst)
                    .to_string(),
                subentry_count: 0,
                num_sponsoring: 0,
                num_sponsored: 0,
                balances: vec![],
            }))
        }
        async fn strict_receive_paths(
            &self,
            _: &str,
            _: &[Asset],
            _: &Asset,
            _: i64,
        ) -> reserve_horizon::Result<Vec<PathRecord>> {
            unimplemented!()
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
        async fn transaction(&self, _: &str) -> reserve_horizon::Result<Option<TransactionRecord>> {
            unimplemented!()
        }
        async fn claimable_balance(
            &self,
            _: &str,
        ) -> reserve_horizon::Result<Option<ClaimableBalance>> {
            unimplemented!()
        }
    }

    fn pool(n: u8) -> ChannelPool {
        ChannelPool::new((0..n).map(|i| Sponsor::from_seed_bytes([i; 32])).collect())
    }

    #[tokio::test]
    async fn a_second_lease_does_not_reuse_the_sequence() {
        let pool = pool(1);
        let horizon = FakeHorizon {
            sequence: std::sync::atomic::AtomicI64::new(10),
        };
        let (a, seq_a) = pool.lease(&horizon, Duration::from_secs(60)).await.unwrap();
        assert_eq!(seq_a, 11);
        assert!(matches!(
            pool.lease(&horizon, Duration::from_secs(60)).await,
            Err(ChannelError::Busy)
        ));
        pool.release(&a, seq_a).await;
        horizon
            .sequence
            .store(11, std::sync::atomic::Ordering::SeqCst);
        let (_, seq_b) = pool.lease(&horizon, Duration::from_secs(60)).await.unwrap();
        assert_eq!(seq_b, 12);
    }

    #[tokio::test]
    async fn two_lanes_can_be_leased_at_once() {
        let pool = pool(2);
        let horizon = FakeHorizon {
            sequence: std::sync::atomic::AtomicI64::new(1),
        };
        let first = pool.lease(&horizon, Duration::from_secs(60)).await.unwrap();
        let second = pool.lease(&horizon, Duration::from_secs(60)).await.unwrap();
        assert_ne!(first.0, second.0);
        assert!(matches!(
            pool.lease(&horizon, Duration::from_secs(60)).await,
            Err(ChannelError::Busy)
        ));
    }

    #[tokio::test]
    async fn an_expired_lease_is_free() {
        let pool = pool(1);
        let horizon = FakeHorizon {
            sequence: std::sync::atomic::AtomicI64::new(4),
        };
        let _ = pool
            .lease(&horizon, Duration::from_millis(1))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let (_, seq) = pool.lease(&horizon, Duration::from_secs(60)).await.unwrap();
        assert_eq!(seq, 5);
    }
}
