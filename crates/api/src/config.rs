use std::env;
use std::sync::Arc;

use reserve_core::pricing::PricingConfig;
use reserve_core::tokens::Allowlist;

use crate::apikey::{KeyIssuer, KeyVerifier};
use crate::limits::LimitsConfig;
use reserve_signer::{Sponsor, PUBLIC_NETWORK, TESTNET};

/// One process can carry both Stellar networks. Each lane is its own sponsor,
/// Horizon, allowlist and rate-limit buckets. Shared: the quote HMAC, the key
/// issuer, bind address, and whether to trust `X-Forwarded-For`.
pub struct Boot {
    pub bind: String,
    pub lanes: Vec<Lane>,
}

pub struct Lane {
    pub slug: &'static str,
    pub config: Config,
}

#[derive(Clone)]
pub struct Config {
    pub network_passphrase: String,
    pub horizon_url: String,
    pub sponsor: Sponsor,
    pub quote_key: Vec<u8>,
    pub pricing: PricingConfig,
    /// Tokens we are willing to be paid in. Never open by default.
    pub token_allowlist: Allowlist,
    pub limits: LimitsConfig,
    /// Verifies integrator keys offline. Holds only the public half, so a
    /// compromise here cannot mint keys.
    pub keys: Arc<KeyVerifier>,
    /// Present only where keys are handed out. Without it the service verifies
    /// keys but cannot mint them, which is what a self-hosted copy wants.
    pub key_issuer: Option<Arc<KeyIssuer>>,
    /// Whether `X-Forwarded-For` may be believed. Only true behind a proxy
    /// that sets it, because a caller can otherwise pick their own rate-limit
    /// bucket by sending whatever address they like.
    pub trust_proxy: bool,
    /// Extra bootstrap lanes. Empty means the sponsor is the only lane.
    pub channel_secrets: Vec<Sponsor>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing environment variable {0}")]
    Missing(&'static str),
    #[error("invalid {0}: {1}")]
    Invalid(&'static str, String),
}

struct Shared {
    bind: String,
    quote_key: Vec<u8>,
    pricing: PricingConfig,
    limits: LimitsConfig,
    key_issuer: Option<Arc<KeyIssuer>>,
    issuer_public: Option<String>,
    trust_proxy: bool,
}

impl Boot {
    pub fn from_env() -> Result<Boot, ConfigError> {
        let shared = Shared::from_env()?;
        let mut lanes = Vec::new();

        if let Some(config) = lane_from_prefix(
            &shared,
            TESTNET,
            "https://horizon-testnet.stellar.org",
            "RESERVE_TESTNET_SPONSOR_SECRET",
            "RESERVE_TESTNET_SPONSOR_SECRET_FILE",
            "RESERVE_TESTNET_HORIZON_URL",
            "RESERVE_TESTNET_TOKENS",
            "RESERVE_TESTNET_CHANNEL_SECRETS",
            "RESERVE_TESTNET_CHANNEL_SECRETS_FILE",
        )? {
            lanes.push(Lane {
                slug: "testnet",
                config,
            });
        }

        if let Some(config) = lane_from_prefix(
            &shared,
            PUBLIC_NETWORK,
            "https://horizon.stellar.org",
            "RESERVE_MAINNET_SPONSOR_SECRET",
            "RESERVE_MAINNET_SPONSOR_SECRET_FILE",
            "RESERVE_MAINNET_HORIZON_URL",
            "RESERVE_MAINNET_TOKENS",
            "RESERVE_MAINNET_CHANNEL_SECRETS",
            "RESERVE_MAINNET_CHANNEL_SECRETS_FILE",
        )? {
            lanes.push(Lane {
                slug: "mainnet",
                config,
            });
        }

        if lanes.is_empty() {
            lanes.push(legacy_lane(&shared)?);
        }

        Ok(Boot {
            bind: shared.bind,
            lanes,
        })
    }
}

impl Shared {
    fn from_env() -> Result<Shared, ConfigError> {
        let quote_key = match env::var("RESERVE_QUOTE_KEY_FILE") {
            Ok(path) => std::fs::read_to_string(&path)
                .map_err(|e| ConfigError::Invalid("RESERVE_QUOTE_KEY_FILE", e.to_string()))?,
            Err(_) => env::var("RESERVE_QUOTE_KEY")
                .map_err(|_| ConfigError::Missing("RESERVE_QUOTE_KEY"))?,
        }
        .trim()
        .to_string()
        .into_bytes();
        if quote_key.len() < 32 {
            return Err(ConfigError::Invalid(
                "RESERVE_QUOTE_KEY",
                "needs at least 32 bytes".into(),
            ));
        }

        let key_issuer = match env::var("RESERVE_KEY_ISSUER_SECRET_FILE")
            .ok()
            .map(|path| std::fs::read_to_string(&path))
            .transpose()
            .map_err(|e| ConfigError::Invalid("RESERVE_KEY_ISSUER_SECRET_FILE", e.to_string()))?
            .or_else(|| env::var("RESERVE_KEY_ISSUER_SECRET").ok())
        {
            Some(secret) => {
                Some(Arc::new(KeyIssuer::new(&secret).map_err(|e| {
                    ConfigError::Invalid("RESERVE_KEY_ISSUER_SECRET", e)
                })?))
            }
            None => None,
        };
        let issuer_public = env::var("RESERVE_KEY_ISSUER_PUBKEY")
            .ok()
            .or_else(|| key_issuer.as_ref().map(|i| i.public_key_hex()));

        Ok(Shared {
            bind: env::var("RESERVE_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            quote_key,
            pricing: PricingConfig {
                margin_bps: env_u32("RESERVE_MARGIN_BPS", 2_000)?,
                slippage_bps: env_u32("RESERVE_SLIPPAGE_BPS", 100)?,
                validity_ledgers: env_u32("RESERVE_QUOTE_LEDGERS", 12)?,
                min_charge_stroops: env_i64("RESERVE_MIN_CHARGE_STROOPS", 56_000)?,
            },
            limits: LimitsConfig {
                anon_per_minute: env_u32("RESERVE_RATE_ANON_PER_MIN", 30)?,
                keyed_per_minute: env_u32("RESERVE_RATE_KEYED_PER_MIN", 600)?,
                global_per_minute: env_u32("RESERVE_RATE_GLOBAL_PER_MIN", 3_000)?,
            },
            key_issuer,
            issuer_public,
            trust_proxy: env::var("RESERVE_TRUST_PROXY")
                .map(|v| v == "true")
                .unwrap_or(false),
        })
    }
}

fn lane_from_prefix(
    shared: &Shared,
    passphrase: &str,
    default_horizon: &str,
    secret_key: &'static str,
    secret_file: &'static str,
    horizon_key: &'static str,
    tokens_key: &'static str,
    channels_key: &'static str,
    channels_file: &'static str,
) -> Result<Option<Config>, ConfigError> {
    let Some(secret) = optional_secret(secret_key, secret_file)? else {
        return Ok(None);
    };
    Ok(Some(build_lane(
        shared,
        passphrase,
        secret,
        env::var(horizon_key).unwrap_or_else(|_| default_horizon.into()),
        env::var(tokens_key).unwrap_or_default(),
        parse_channel_secrets(channels_key, channels_file)?,
        secret_key,
        tokens_key,
    )?))
}

fn legacy_lane(shared: &Shared) -> Result<Lane, ConfigError> {
    let network = env::var("RESERVE_NETWORK").unwrap_or_else(|_| "testnet".into());
    let (slug, passphrase, default_horizon) = match network.as_str() {
        "mainnet" => (
            "mainnet",
            PUBLIC_NETWORK,
            "https://horizon.stellar.org",
        ),
        "testnet" => (
            "testnet",
            TESTNET,
            "https://horizon-testnet.stellar.org",
        ),
        other => return Err(ConfigError::Invalid("RESERVE_NETWORK", other.into())),
    };
    let secret = optional_secret("RESERVE_SPONSOR_SECRET", "RESERVE_SPONSOR_SECRET_FILE")?
        .ok_or(ConfigError::Missing("RESERVE_SPONSOR_SECRET"))?;
    let config = build_lane(
        shared,
        passphrase,
        secret,
        env::var("RESERVE_HORIZON_URL").unwrap_or_else(|_| default_horizon.into()),
        env::var("RESERVE_TOKENS").unwrap_or_default(),
        parse_channel_secrets("RESERVE_CHANNEL_SECRETS", "RESERVE_CHANNEL_SECRETS_FILE")?,
        "RESERVE_SPONSOR_SECRET",
        "RESERVE_TOKENS",
    )?;
    Ok(Lane { slug, config })
}

fn build_lane(
    shared: &Shared,
    passphrase: &str,
    secret: String,
    horizon_url: String,
    configured_tokens: String,
    channel_secrets: Vec<Sponsor>,
    secret_key: &'static str,
    tokens_key: &'static str,
) -> Result<Config, ConfigError> {
    let sponsor = Sponsor::from_strkey(secret.trim())
        .map_err(|e| ConfigError::Invalid(secret_key, e.to_string()))?;

    let token_allowlist = Allowlist::parse(&configured_tokens, passphrase);
    if token_allowlist == Allowlist::Any && passphrase == PUBLIC_NETWORK {
        return Err(ConfigError::Invalid(
            tokens_key,
            "the wildcard is not accepted on mainnet: name the assets to accept"
                .into(),
        ));
    }

    let keys = KeyVerifier::new(shared.issuer_public.as_deref(), passphrase.to_string())
        .map_err(|e| ConfigError::Invalid("RESERVE_KEY_ISSUER_PUBKEY", e))?;

    Ok(Config {
        network_passphrase: passphrase.to_string(),
        horizon_url,
        sponsor,
        quote_key: shared.quote_key.clone(),
        pricing: shared.pricing.clone(),
        token_allowlist,
        limits: shared.limits.clone(),
        keys: Arc::new(keys),
        key_issuer: shared.key_issuer.clone(),
        trust_proxy: shared.trust_proxy,
        channel_secrets,
    })
}

fn optional_secret(
    value_key: &'static str,
    file_key: &'static str,
) -> Result<Option<String>, ConfigError> {
    match env::var(file_key) {
        Ok(path) => Ok(Some(
            std::fs::read_to_string(&path)
                .map_err(|e| ConfigError::Invalid(file_key, e.to_string()))?,
        )),
        Err(_) => Ok(env::var(value_key).ok().filter(|s| !s.trim().is_empty())),
    }
}

fn parse_channel_secrets(
    value_key: &'static str,
    file_key: &'static str,
) -> Result<Vec<Sponsor>, ConfigError> {
    let raw = match env::var(file_key) {
        Ok(path) => std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::Invalid(file_key, e.to_string()))?,
        Err(_) => env::var(value_key).unwrap_or_default(),
    };
    raw.split(|c| c == ',' || c == '\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            Sponsor::from_strkey(s).map_err(|e| ConfigError::Invalid(value_key, e.to_string()))
        })
        .collect()
}

fn env_i64(key: &'static str, default: i64) -> Result<i64, ConfigError> {
    match env::var(key) {
        Ok(v) => v.parse().map_err(|_| ConfigError::Invalid(key, v)),
        Err(_) => Ok(default),
    }
}

fn env_u32(key: &'static str, default: u32) -> Result<u32, ConfigError> {
    match env::var(key) {
        Ok(v) => v.parse().map_err(|_| ConfigError::Invalid(key, v)),
        Err(_) => Ok(default),
    }
}
