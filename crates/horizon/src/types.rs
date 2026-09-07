use serde::Deserialize;

/// A Stellar asset as Horizon renders it in query parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asset {
    Native,
    Credit { code: String, issuer: String },
}

impl Asset {
    /// Canonical `CODE:ISSUER` / `native` form used across the API surface.
    pub fn canonical(&self) -> String {
        match self {
            Asset::Native => "native".to_string(),
            Asset::Credit { code, issuer } => format!("{code}:{issuer}"),
        }
    }

    pub fn parse(s: &str) -> Option<Asset> {
        if s == "native" {
            return Some(Asset::Native);
        }
        let (code, issuer) = s.split_once(':')?;
        if code.is_empty() || code.len() > 12 || !issuer.starts_with('G') {
            return None;
        }
        Some(Asset::Credit {
            code: code.to_string(),
            issuer: issuer.to_string(),
        })
    }

    /// Horizon takes the type/code/issuer triple as separate query params.
    pub fn query_triple(&self, prefix: &str) -> Vec<(String, String)> {
        match self {
            Asset::Native => vec![(format!("{prefix}_asset_type"), "native".into())],
            Asset::Credit { code, issuer } => {
                let ty = if code.len() <= 4 {
                    "credit_alphanum4"
                } else {
                    "credit_alphanum12"
                };
                vec![
                    (format!("{prefix}_asset_type"), ty.into()),
                    (format!("{prefix}_asset_code"), code.clone()),
                    (format!("{prefix}_asset_issuer"), issuer.clone()),
                ]
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountBalance {
    pub balance: String,
    pub asset_type: String,
    #[serde(default)]
    pub asset_code: Option<String>,
    #[serde(default)]
    pub asset_issuer: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub id: String,
    /// Horizon serialises the sequence number as a string.
    pub sequence: String,
    pub subentry_count: u32,
    #[serde(default)]
    pub num_sponsoring: u32,
    #[serde(default)]
    pub num_sponsored: u32,
    #[serde(default)]
    pub balances: Vec<AccountBalance>,
}

impl Account {
    pub fn sequence_i64(&self) -> Option<i64> {
        self.sequence.parse().ok()
    }

    pub fn native_balance_stroops(&self) -> Option<i64> {
        self.balances
            .iter()
            .find(|b| b.asset_type == "native")
            .and_then(|b| parse_amount_stroops(&b.balance))
    }
}

/// One candidate route returned by strict-receive path finding.
#[derive(Debug, Clone, Deserialize)]
pub struct PathRecord {
    pub source_amount: String,
    pub destination_amount: String,
    pub source_asset_type: String,
    #[serde(default)]
    pub source_asset_code: Option<String>,
    #[serde(default)]
    pub source_asset_issuer: Option<String>,
    #[serde(default)]
    pub path: Vec<PathHop>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathHop {
    pub asset_type: String,
    #[serde(default)]
    pub asset_code: Option<String>,
    #[serde(default)]
    pub asset_issuer: Option<String>,
}

impl PathHop {
    pub fn to_asset(&self) -> Option<Asset> {
        if self.asset_type == "native" {
            return Some(Asset::Native);
        }
        Some(Asset::Credit {
            code: self.asset_code.clone()?,
            issuer: self.asset_issuer.clone()?,
        })
    }
}

impl PathRecord {
    pub fn source_amount_stroops(&self) -> Option<i64> {
        parse_amount_stroops(&self.source_amount)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Embedded<T> {
    pub records: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Wrapper<T> {
    pub _embedded: Embedded<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeeStats {
    pub last_ledger_base_fee: String,
    pub fee_charged: FeeBucket,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeeBucket {
    pub min: String,
    pub mode: String,
    pub p50: String,
    pub p90: String,
    pub p99: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ledger {
    pub sequence: u32,
    pub base_fee_in_stroops: i64,
    pub base_reserve_in_stroops: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TransactionRecord {
    pub hash: String,
    pub ledger: u32,
    pub successful: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Claimant {
    pub destination: String,
    #[serde(default)]
    pub predicate: crate::Predicate,
}

/// One claimable balance, as `GET /claimable_balances/{id}` returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimableBalance {
    pub id: String,
    pub asset: String,
    pub amount: String,
    #[serde(default)]
    pub claimants: Vec<Claimant>,
    #[serde(default)]
    pub last_modified_time: Option<String>,
}

impl ClaimableBalance {
    pub fn amount_stroops(&self) -> Option<i64> {
        parse_amount_stroops(&self.amount)
    }

    /// Whether `address` is a claimant whose predicate holds at `now`.
    pub fn claimable_by(&self, address: &str, now: i64) -> bool {
        let created = self
            .last_modified_time
            .as_deref()
            .and_then(crate::parse_rfc3339_utc);
        self.claimants
            .iter()
            .any(|c| c.destination == address && c.predicate.holds(now, created))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitResponse {
    pub hash: String,
    pub ledger: Option<u32>,
    #[serde(default)]
    pub successful: Option<bool>,
}

/// Horizon amounts are decimal strings with 7 places; convert to stroops
/// without going through floating point.
pub fn parse_amount_stroops(amount: &str) -> Option<i64> {
    let (whole, frac) = match amount.split_once('.') {
        Some((w, f)) => (w, f),
        None => (amount, ""),
    };
    if frac.len() > 7
        || !whole.chars().all(|c| c.is_ascii_digit())
        || !frac.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let padded = format!("{frac:0<7}");
    let whole: i64 = whole.parse().ok()?;
    let frac: i64 = if padded.is_empty() {
        0
    } else {
        padded.parse().ok()?
    };
    whole.checked_mul(10_000_000)?.checked_add(frac)
}

/// Inverse of [`parse_amount_stroops`].
pub fn format_stroops(stroops: i64) -> String {
    let sign = if stroops < 0 { "-" } else { "" };
    let v = stroops.unsigned_abs();
    format!("{sign}{}.{:07}", v / 10_000_000, v % 10_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_round_trip() {
        assert_eq!(parse_amount_stroops("1.0000000"), Some(10_000_000));
        assert_eq!(parse_amount_stroops("0.0000001"), Some(1));
        assert_eq!(parse_amount_stroops("12"), Some(120_000_000));
        assert_eq!(parse_amount_stroops("0.00000001"), None);
        assert_eq!(format_stroops(10_000_001), "1.0000001");
        assert_eq!(format_stroops(1), "0.0000001");
    }

    #[test]
    fn asset_parsing() {
        assert_eq!(Asset::parse("native"), Some(Asset::Native));
        let usdc =
            Asset::parse("USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN").unwrap();
        assert_eq!(
            usdc.canonical(),
            "USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN"
        );
        assert_eq!(usdc.query_triple("source")[0].1, "credit_alphanum4");
        assert!(Asset::parse("USDC:nope").is_none());
    }
}
