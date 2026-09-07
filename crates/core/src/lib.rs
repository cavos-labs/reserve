//! Reserve domain logic: planning, pricing, building and validating the
//! transactions the service sponsors.
//!
//! The invariant that holds the whole product together: the server is the only
//! thing that builds the inner transaction, and at submit time it **rebuilds it
//! from the quote and compares the XDR byte for byte**. Nothing is trusted from
//! the client except the signatures.

pub mod asset;
pub mod build;
pub mod claim;
pub mod numeric;
pub mod path;
pub mod plan;
pub mod pricing;
pub mod quote;
pub mod tokens;
pub mod validate;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid asset: {0}")]
    Asset(String),
    #[error("invalid account: {0}")]
    Account(String),
    #[error("invalid amount: {0}")]
    Amount(String),
    #[error("no path from {token} to XLM with enough liquidity")]
    NoPath { token: String },
    #[error("quote expired at ledger {expires_at_ledger}")]
    QuoteExpired { expires_at_ledger: u32 },
    #[error("quote signature mismatch")]
    QuoteSignature,
    #[error("submitted transaction does not match the quote: {0}")]
    Mismatch(String),
    #[error("unclaimable: {0}")]
    Unclaimable(String),
    #[error("path moved: {0}")]
    PathMoved(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("xdr: {0}")]
    Xdr(String),
    #[error(transparent)]
    Horizon(#[from] reserve_horizon::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Stroops per lumen.
pub const STROOPS_PER_XLM: i64 = 10_000_000;
