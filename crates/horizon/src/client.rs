use std::time::Duration;

use crate::types::{
    Account, Asset, ClaimableBalance, FeeStats, Ledger, PathRecord, SubmitResponse,
    TransactionRecord, Wrapper,
};
use crate::{Error, Result};

/// The Horizon surface the rest of the service depends on. Kept narrow on
/// purpose: only what quoting, building and submitting actually need.
#[allow(async_fn_in_trait)]
pub trait HorizonApi {
    async fn account(&self, id: &str) -> Result<Option<Account>>;
    async fn strict_receive_paths(
        &self,
        source_account: &str,
        source_assets: &[Asset],
        destination: &Asset,
        destination_amount_stroops: i64,
    ) -> Result<Vec<PathRecord>>;
    async fn fee_stats(&self) -> Result<FeeStats>;
    async fn latest_ledger(&self) -> Result<Ledger>;
    async fn submit(&self, envelope_xdr_base64: &str) -> Result<SubmitResponse>;
    /// A transaction that has already closed, or `None` while it has not.
    async fn transaction(&self, hash: &str) -> Result<Option<TransactionRecord>>;
    /// `None` if Horizon has no such balance (404).
    async fn claimable_balance(&self, id: &str) -> Result<Option<ClaimableBalance>>;
}

#[derive(Clone)]
pub struct Horizon {
    base: String,
    http: reqwest::Client,
}

impl Horizon {
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("reserve/0.1")
            .build()
            .expect("reqwest client");
        Self {
            base: base_url.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<Option<T>> {
        let url = format!("{}{}", self.base, path);
        let res = self.http.get(&url).query(query).send().await?;
        let status = res.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        let body = res.text().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                body,
            });
        }
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| Error::Decode(e.to_string()))
    }
}

impl HorizonApi for Horizon {
    async fn account(&self, id: &str) -> Result<Option<Account>> {
        self.get_json(&format!("/accounts/{id}"), &[]).await
    }

    async fn strict_receive_paths(
        &self,
        source_account: &str,
        source_assets: &[Asset],
        destination: &Asset,
        destination_amount_stroops: i64,
    ) -> Result<Vec<PathRecord>> {
        let mut query = destination.query_triple("destination");
        query.push((
            "destination_amount".into(),
            crate::types::format_stroops(destination_amount_stroops),
        ));
        // Horizon accepts either a source account (uses its balances) or an
        // explicit asset list. The asset list keeps quoting deterministic.
        if source_assets.is_empty() {
            query.push(("source_account".into(), source_account.to_string()));
        } else {
            let list = source_assets
                .iter()
                .map(Asset::canonical)
                .collect::<Vec<_>>()
                .join(",");
            query.push(("source_assets".into(), list));
        }
        let wrapped: Option<Wrapper<PathRecord>> =
            self.get_json("/paths/strict-receive", &query).await?;
        Ok(wrapped.map(|w| w._embedded.records).unwrap_or_default())
    }

    async fn fee_stats(&self) -> Result<FeeStats> {
        self.get_json("/fee_stats", &[])
            .await?
            .ok_or_else(|| Error::NotFound("fee_stats".into()))
    }

    async fn latest_ledger(&self) -> Result<Ledger> {
        let wrapped: Option<Wrapper<Ledger>> = self
            .get_json(
                "/ledgers",
                &[
                    ("order".into(), "desc".into()),
                    ("limit".into(), "1".into()),
                ],
            )
            .await?;
        wrapped
            .and_then(|w| w._embedded.records.into_iter().next())
            .ok_or_else(|| Error::NotFound("ledgers".into()))
    }

    async fn transaction(&self, hash: &str) -> Result<Option<TransactionRecord>> {
        self.get_json(&format!("/transactions/{hash}"), &[]).await
    }

    async fn claimable_balance(&self, id: &str) -> Result<Option<ClaimableBalance>> {
        self.get_json(&format!("/claimable_balances/{id}"), &[])
            .await
    }

    async fn submit(&self, envelope_xdr_base64: &str) -> Result<SubmitResponse> {
        let url = format!("{}/transactions", self.base);
        let res = self
            .http
            .post(&url)
            .form(&[("tx", envelope_xdr_base64)])
            .send()
            .await?;
        let status = res.status();
        let body = res.text().await?;
        if !status.is_success() {
            // Horizon puts tx_* result codes in the problem body; pass it
            // through verbatim so callers can act on the real reason.
            return Err(Error::Api {
                status: status.as_u16(),
                body,
            });
        }
        serde_json::from_str(&body).map_err(|e| Error::Decode(e.to_string()))
    }
}
