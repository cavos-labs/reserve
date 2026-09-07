//! Counters, kept in memory.
//!
//! Nothing here is persisted: a restart resets the counters, which is what
//! every metrics scraper already expects, and it keeps the service free of
//! storage.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Metrics {
    quotes: AtomicU64,
    submissions: AtomicU64,
    failures: AtomicU64,
    rate_limited: AtomicU64,
}

pub struct Snapshot {
    pub quotes: u64,
    pub submissions: u64,
    pub failures: u64,
    pub rate_limited: u64,
}

impl Metrics {
    pub fn quoted(&self) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    pub fn submitted(&self) {
        self.submissions.fetch_add(1, Ordering::Relaxed);
    }
    pub fn failed(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
    }
    pub fn rate_limited(&self) {
        self.rate_limited.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            quotes: self.quotes.load(Ordering::Relaxed),
            submissions: self.submissions.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
        }
    }
}
