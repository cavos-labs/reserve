//! Small helpers for driving testnet directly: friendbot funding and the
//! handful of classic operations the fixture needs (payments, trustlines and
//! an offer to give the fee token a route to XLM).

use anyhow::{anyhow, Context, Result};
use reserve_horizon::{Horizon, HorizonApi};
use reserve_signer::Sponsor;
use stellar_xdr as xdr;
use xdr::{Limits, WriteXdr};

pub const TESTNET_HORIZON: &str = "https://horizon-testnet.stellar.org";
pub const FRIENDBOT: &str = "https://friendbot.stellar.org";

pub fn random_keypair() -> Sponsor {
    let seed: [u8; 32] = rand::random();
    Sponsor::from_seed_bytes(seed)
}

pub async fn friendbot(address: &str) -> Result<()> {
    let res = reqwest::Client::new()
        .get(FRIENDBOT)
        .query(&[("addr", address)])
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(anyhow!("friendbot {}: {}", res.status(), res.text().await?));
    }
    Ok(())
}

pub fn asset(code: &str, issuer: &str) -> Result<xdr::Asset> {
    let a = reserve_core::asset::parse_asset(&format!("{code}:{issuer}"))?;
    Ok(reserve_core::asset::to_xdr_asset(&a)?)
}

pub fn op(body: xdr::OperationBody) -> xdr::Operation {
    xdr::Operation {
        source_account: None,
        body,
    }
}

/// Build, sign and submit a transaction sourced by `source`.
pub async fn send(
    horizon: &Horizon,
    source: &Sponsor,
    ops: Vec<xdr::Operation>,
    extra_signers: &[&Sponsor],
) -> Result<String> {
    let account = horizon
        .account(&source.address())
        .await?
        .ok_or_else(|| anyhow!("account {} does not exist", source.address()))?;
    let seq = account.sequence_i64().context("sequence")? + 1;
    let fee = 100 * ops.len() as u32;
    let tx = xdr::Transaction {
        source_account: source.muxed_account(),
        fee,
        seq_num: xdr::SequenceNumber(seq),
        cond: xdr::Preconditions::Time(xdr::TimeBounds {
            min_time: xdr::TimePoint(0),
            max_time: xdr::TimePoint(now() + 180),
        }),
        memo: xdr::Memo::None,
        operations: ops.try_into().map_err(|_| anyhow!("too many ops"))?,
        ext: xdr::TransactionExt::V0,
    };
    let mut envelope = match source.sign_transaction(tx, reserve_signer::TESTNET)? {
        xdr::TransactionEnvelope::Tx(v1) => v1,
        _ => unreachable!("sign_transaction returns a v1 envelope"),
    };
    for signer in extra_signers {
        envelope = match signer.co_sign(envelope, reserve_signer::TESTNET)? {
            xdr::TransactionEnvelope::Tx(v1) => v1,
            _ => unreachable!(),
        };
    }
    let xdr_b64 = xdr::TransactionEnvelope::Tx(envelope).to_xdr_base64(Limits::none())?;
    let res = horizon.submit(&xdr_b64).await?;
    Ok(res.hash)
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}
