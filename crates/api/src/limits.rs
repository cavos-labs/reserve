//! Abuse control.
//!
//! Two different things are worth protecting, and they need different limits.
//!
//! The first is the upstream quota. Quoting is free for the caller and costly
//! for us: every quote does path finding against Horizon. A loop of unsigned
//! quote requests never touches a key, never spends a stroop, and can still
//! exhaust the rate limit the whole service shares. That is the cheapest way
//! to take the service down, so it gets the tightest limit.
//!
//! The sponsor's XLM is defended in two other places, not here. A failed
//! *sponsored* submission costs the fee bump and needs a funded account. A
//! failed *bootstrap* submission does not: the sponsor is the source, so an
//! unfunded caller with a well-formed `balance_id` used to burn a sequence
//! number. That hole is closed by checking the claimable balance against
//! Horizon before we sign, not by a stroop breaker in this file.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::time::Duration;

use governor::clock::DefaultClock;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};

type Keyed<K> = RateLimiter<K, DefaultKeyedStateStore<K>, DefaultClock>;

/// Who is calling, for the purpose of limiting them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Caller {
    /// A known integrator, identified by API key. Not billed; the key exists
    /// to give them their own budget rather than sharing the anonymous one.
    Key(String),
    /// Anyone else, bucketed by address.
    Anonymous(IpAddr),
}

#[derive(Debug, Clone)]
pub struct LimitsConfig {
    pub anon_per_minute: u32,
    pub keyed_per_minute: u32,
    pub global_per_minute: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            anon_per_minute: 30,
            keyed_per_minute: 600,
            global_per_minute: 3_000,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Retry after roughly this many seconds.
    Deny {
        retry_after: u64,
        reason: &'static str,
    },
}

pub struct Limits {
    // Two limiters rather than one keyed by `Caller`: a keyed store carries a
    // single quota for every key it holds, and integrators are meant to have a
    // different one from the street.
    keyed: Keyed<String>,
    anonymous: Keyed<IpAddr>,
    global: governor::DefaultDirectRateLimiter,
    config: LimitsConfig,
}

fn quota(per_minute: u32) -> Quota {
    let n = NonZeroU32::new(per_minute.max(1)).expect("non-zero");
    // Allow a full minute's worth as burst: clients legitimately arrive in
    // bunches, and the sustained rate is what matters.
    Quota::per_minute(n).allow_burst(n)
}

impl Limits {
    pub fn new(config: LimitsConfig) -> Limits {
        Limits {
            keyed: RateLimiter::keyed(quota(config.keyed_per_minute)),
            anonymous: RateLimiter::keyed(quota(config.anon_per_minute)),
            global: RateLimiter::direct(quota(config.global_per_minute)),
            config,
        }
    }

    /// Check a request from `caller`. Keyed callers get their own, larger
    /// budget; everyone else shares the anonymous rate per address.
    ///
    /// The per-caller check runs first. `governor`'s `check` consumes a cell
    /// when it allows, so doing the global check first let a single denied
    /// address drain the shared budget.
    pub fn check(&self, caller: &Caller) -> Decision {
        let allowed = match caller {
            Caller::Key(key) => self.keyed.check_key(key).is_ok(),
            Caller::Anonymous(ip) => self.anonymous.check_key(ip).is_ok(),
        };
        if !allowed {
            return Decision::Deny {
                retry_after: 60,
                reason: "too many requests",
            };
        }
        if self.global.check().is_err() {
            return Decision::Deny {
                retry_after: 5,
                reason: "the service is at capacity",
            };
        }
        Decision::Allow
    }

    /// Drop keyed state that has gone quiet, so memory does not grow with the
    /// number of addresses ever seen.
    pub fn prune(&self) {
        self.keyed.retain_recent();
        self.anonymous.retain_recent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> Caller {
        Caller::Anonymous(IpAddr::from([10, 0, 0, n]))
    }

    fn limits() -> Limits {
        Limits::new(LimitsConfig {
            anon_per_minute: 3,
            keyed_per_minute: 30,
            global_per_minute: 1_000,
        })
    }

    #[test]
    fn anonymous_callers_get_their_own_budget() {
        let limits = limits();
        for _ in 0..3 {
            assert_eq!(limits.check(&ip(1)), Decision::Allow);
        }
        assert!(matches!(limits.check(&ip(1)), Decision::Deny { .. }));
        // One noisy address does not affect anyone else.
        assert_eq!(limits.check(&ip(2)), Decision::Allow);
    }

    #[test]
    fn a_key_buys_more_headroom_than_an_address() {
        let limits = limits();
        let key = Caller::Key("integrator".into());
        // Ten times the anonymous budget, so the anonymous ceiling is not it.
        for i in 0..10 {
            assert_eq!(limits.check(&key), Decision::Allow, "request {i}");
        }
    }

    #[test]
    fn the_global_ceiling_stops_everything() {
        let limits = Limits::new(LimitsConfig {
            global_per_minute: 2,
            ..LimitsConfig::default()
        });
        assert_eq!(limits.check(&ip(1)), Decision::Allow);
        assert_eq!(limits.check(&ip(2)), Decision::Allow);
        // Different addresses, but the upstream quota is shared.
        assert!(matches!(limits.check(&ip(3)), Decision::Deny { .. }));
    }

    #[test]
    fn a_denied_caller_does_not_eat_the_global_budget() {
        let limits = Limits::new(LimitsConfig {
            anon_per_minute: 1,
            keyed_per_minute: 30,
            global_per_minute: 2,
        });
        assert_eq!(limits.check(&ip(1)), Decision::Allow);
        for _ in 0..20 {
            assert!(matches!(limits.check(&ip(1)), Decision::Deny { .. }));
        }
        // The remaining global cell is still there for someone else.
        assert_eq!(limits.check(&ip(2)), Decision::Allow);
    }

    #[test]
    fn ipv6_addresses_in_the_same_slash_64_share_a_bucket() {
        let a = bucket_ip(IpAddr::from(std::net::Ipv6Addr::new(
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 1,
        )));
        let b = bucket_ip(IpAddr::from(std::net::Ipv6Addr::new(
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 2,
        )));
        let other = bucket_ip(IpAddr::from(std::net::Ipv6Addr::new(
            0x2001, 0xdb8, 1, 0, 0, 0, 0, 1,
        )));
        assert_eq!(a, b);
        assert_ne!(a, other);

        let limits = Limits::new(LimitsConfig {
            anon_per_minute: 1,
            keyed_per_minute: 30,
            global_per_minute: 1_000,
        });
        assert_eq!(limits.check(&Caller::Anonymous(a)), Decision::Allow);
        assert!(matches!(
            limits.check(&Caller::Anonymous(b)),
            Decision::Deny { .. }
        ));
        assert_eq!(limits.check(&Caller::Anonymous(other)), Decision::Allow);
    }
}

/// Collapse an address to the unit we rate-limit on.
///
/// IPv4 stays as-is. IPv6 is grouped by /64 — the assignment an end site
/// typically receives — so a single subscriber cannot mint a fresh bucket per
/// address. IPv4-mapped IPv6 (`:ffff:a.b.c.d`) is treated as IPv4 so the two
/// encodings of the same host share a budget.
pub fn bucket_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v) => IpAddr::V4(v),
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let mut octets = v.octets();
            octets[8..].fill(0);
            IpAddr::V6(std::net::Ipv6Addr::from(octets))
        }
    }
}

/// Drop rate-limit state for callers that have gone quiet, so memory does not
/// grow with every address the service has ever seen.
pub fn spawn_pruner(state: std::sync::Arc<crate::state::AppState>, every: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            state.limits.prune();
        }
    });
}
