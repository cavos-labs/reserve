//! Minimal typed Horizon client.
//!
//! Horizon is deprecated in favour of Stellar RPC, but strict-receive path
//! finding only exists on Horizon, so quoting depends on it. Everything is
//! behind `HorizonApi` so the path-finding half can be swapped later.

mod client;
mod predicate;
mod types;

pub use client::{Horizon, HorizonApi};
pub use predicate::{parse_rfc3339_utc, Predicate};
pub use types::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("horizon {status}: {body}")]
    Api { status: u16, body: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unexpected response: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, Error>;
