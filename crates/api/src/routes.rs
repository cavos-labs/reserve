use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use reserve_core::asset::parse_asset;
use reserve_core::build::build_inner;
use reserve_core::plan::{Mode, Plan, UserOp};
use reserve_core::pricing;
use reserve_core::quote::Quote;
use reserve_core::validate::check_signed_inner;
use reserve_horizon::HorizonApi;

use crate::channels::ChannelError;
use crate::limits::{Caller, Decision};
use crate::state::AppState;

/// How long the signed inner transaction stays valid on the network.
const TX_TIMEOUT_SECS: u64 = 180;

/// Site on `/`. Each Stellar network is its own tree under `/testnet` or
/// `/mainnet`. A single-lane process also keeps `/v1` at the root so a local
/// `RESERVE_URL=http://127.0.0.1:8080` still works.
pub fn app(lanes: Vec<(&'static str, Arc<AppState>)>) -> Router {
    let catalog = Arc::new(Catalog {
        lanes: lanes.clone(),
        anon_per_minute: lanes
            .first()
            .map(|(_, s)| s.config.limits.anon_per_minute)
            .unwrap_or(30),
        keyed_per_minute: lanes
            .first()
            .map(|(_, s)| s.config.limits.keyed_per_minute)
            .unwrap_or(600),
    });

    let mut router = site_router();
    for (slug, state) in &lanes {
        router = router.nest(&format!("/{slug}"), api_router(state.clone()));
    }
    if lanes.len() == 1 {
        // One network: keep /v1 and /health at the root so a local
        // RESERVE_URL=http://127.0.0.1:8080 still works.
        router = router.merge(api_router(lanes[0].1.clone()));
    } else {
        router = router.merge(
            Router::new()
                .route("/health", get(catalog_health))
                .with_state(catalog),
        );
    }
    router
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

fn site_router() -> Router {
    Router::new()
        // One page and its typeface, compiled in. A key is not worth a second
        // deployment, and the page should not ask a third party for a font.
        .route(
            "/",
            get(|| async { axum::response::Html(include_str!("../static/index.html")) }),
        )
        .route(
            "/favicon.png",
            get(|| async {
                (
                    [
                        (axum::http::header::CONTENT_TYPE, "image/png"),
                        (axum::http::header::CACHE_CONTROL, "public, max-age=604800"),
                    ],
                    include_bytes!("../static/favicon.png").as_slice(),
                )
            }),
        )
        .route(
            "/wallets.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../static/wallets.js"),
                )
            }),
        )
        .route(
            "/geist.woff2",
            get(|| async {
                (
                    [
                        (axum::http::header::CONTENT_TYPE, "font/woff2"),
                        (
                            axum::http::header::CACHE_CONTROL,
                            "public, max-age=31536000, immutable",
                        ),
                    ],
                    include_bytes!("../static/geist.woff2").as_slice(),
                )
            }),
        )
        .route(
            "/site.css",
            get(|| async {
                (
                    [
                        (
                            axum::http::header::CONTENT_TYPE,
                            "text/css; charset=utf-8",
                        ),
                        (axum::http::header::CACHE_CONTROL, "public, max-age=3600"),
                    ],
                    include_str!("../static/site.css"),
                )
            }),
        )
        .route(
            "/key",
            get(|| async { axum::response::Html(include_str!("../static/key.html")) }),
        )
        .route(
            "/usdc-front.webp",
            get(|| async {
                (
                    [
                        (axum::http::header::CONTENT_TYPE, "image/webp"),
                        (axum::http::header::CACHE_CONTROL, "public, max-age=604800"),
                    ],
                    include_bytes!("../static/usdc-front.webp").as_slice(),
                )
            }),
        )
        .route(
            "/xlm-reveal.webp",
            get(|| async {
                (
                    [
                        (axum::http::header::CONTENT_TYPE, "image/webp"),
                        (axum::http::header::CACHE_CONTROL, "public, max-age=604800"),
                    ],
                    include_bytes!("../static/xlm-reveal.webp").as_slice(),
                )
            }),
        )
        .route(
            "/docs",
            get(|| async { axum::response::Html(include_str!("../static/docs.html")) }),
        )
        .route(
            "/docs/quickstart",
            get(|| async { axum::response::Html(include_str!("../static/docs-quickstart.html")) }),
        )
        .route(
            "/docs/api",
            get(|| async { axum::response::Html(include_str!("../static/docs-api.html")) }),
        )
        .route(
            "/docs/agents",
            get(|| async { axum::response::Html(include_str!("../static/docs-agents.html")) }),
        )
        .route(
            "/llms.txt",
            get(|| async { text_file("text/plain; charset=utf-8", include_str!("../static/llms.txt")) }),
        )
        .route(
            "/llms-full.txt",
            get(|| async {
                text_file(
                    "text/plain; charset=utf-8",
                    include_str!("../static/llms-full.txt"),
                )
            }),
        )
        .route(
            "/openapi.yaml",
            get(|| async {
                text_file(
                    "application/yaml; charset=utf-8",
                    include_str!("../static/openapi.yaml"),
                )
            }),
        )
        .route(
            "/robots.txt",
            get(|| async { text_file("text/plain; charset=utf-8", include_str!("../static/robots.txt")) }),
        )
        .route(
            "/sitemap.xml",
            get(|| async {
                text_file(
                    "application/xml; charset=utf-8",
                    include_str!("../static/sitemap.xml"),
                )
            }),
        )
}

fn text_file(
    content_type: &'static str,
    body: &'static str,
) -> impl axum::response::IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (axum::http::header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
}

fn api_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/tokens", get(tokens))
        .route("/v1/challenge", post(challenge))
        .route("/v1/keys", post(issue_key))
        .route("/v1/quote", post(quote))
        .route("/v1/build", post(build))
        .route("/v1/submit", post(submit))
        .route("/v1/status/{hash}", get(status))
        .route("/metrics", get(metrics))
        .with_state(state)
}

struct Catalog {
    lanes: Vec<(&'static str, Arc<AppState>)>,
    anon_per_minute: u32,
    keyed_per_minute: u32,
}

// ---------------------------------------------------------------- errors

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }
    fn rate_limited(message: &'static str, retry_after: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            message: message.to_string(),
            retry_after: Some(retry_after),
        }
    }
    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
    fn bootstrap_busy() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "bootstrap_busy",
            message: "another bootstrap is using this lane; retry shortly".into(),
            retry_after: Some(2),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(seconds) = self.retry_after {
            if let Ok(value) = seconds.to_string().parse() {
                headers.insert(axum::http::header::RETRY_AFTER, value);
            }
        }
        (
            self.status,
            headers,
            Json(serde_json::json!({ "error": self.code, "message": self.message })),
        )
            .into_response()
    }
}

impl From<std::net::AddrParseError> for ApiError {
    fn from(_: std::net::AddrParseError) -> Self {
        ApiError::bad_request("invalid_request", "unreadable address")
    }
}

impl From<reserve_core::Error> for ApiError {
    fn from(e: reserve_core::Error) -> Self {
        use reserve_core::Error as E;
        let (status, code) = match &e {
            E::QuoteExpired { .. } => (StatusCode::CONFLICT, "quote_expired"),
            E::QuoteSignature => (StatusCode::UNAUTHORIZED, "quote_signature"),
            E::Mismatch(_) => (StatusCode::BAD_REQUEST, "quote_mismatch"),
            E::NoPath { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "no_path"),
            E::Unclaimable(_) => (StatusCode::UNPROCESSABLE_ENTITY, "unclaimable"),
            E::PathMoved(_) => (StatusCode::CONFLICT, "path_moved"),
            E::Horizon(_) => (StatusCode::BAD_GATEWAY, "horizon"),
            _ => (StatusCode::BAD_REQUEST, "invalid_request"),
        };
        ApiError::new(status, code, e.to_string())
    }
}

impl From<reserve_horizon::Error> for ApiError {
    fn from(e: reserve_horizon::Error) -> Self {
        ApiError::new(StatusCode::BAD_GATEWAY, "horizon", e.to_string())
    }
}

impl From<reserve_signer::Error> for ApiError {
    fn from(e: reserve_signer::Error) -> Self {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "signer", e.to_string())
    }
}

impl From<ChannelError> for ApiError {
    fn from(e: ChannelError) -> Self {
        match e {
            ChannelError::Busy => ApiError::bootstrap_busy(),
            ChannelError::Missing(addr) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "channel_missing",
                format!("channel account {addr} does not exist"),
            ),
            ChannelError::Horizon(e) => e.into(),
        }
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

// ---------------------------------------------------------------- handlers

#[derive(Serialize)]
struct Health {
    status: &'static str,
    sponsor: String,
    network: String,
    /// Absent when the chain could not be reached. Health should say the
    /// service is up even when the number behind it is momentarily unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    funds: Option<crate::state::SponsorFunds>,
    /// The unkeyed budget. The page reads this rather than hardcoding 30.
    anon_per_minute: u32,
    /// The keyed budget. A key is a bigger bucket, not a permission.
    keyed_per_minute: u32,
}

async fn health(State(state): State<Arc<AppState>>) -> ApiResult<Health> {
    Ok(Json(lane_health(&state).await))
}

async fn catalog_health(State(catalog): State<Arc<Catalog>>) -> Json<serde_json::Value> {
    let mut networks = serde_json::Map::new();
    for (slug, state) in &catalog.lanes {
        networks.insert(
            (*slug).into(),
            serde_json::to_value(lane_health(state).await).unwrap_or_default(),
        );
    }
    Json(serde_json::json!({
        "status": "ok",
        "anon_per_minute": catalog.anon_per_minute,
        "keyed_per_minute": catalog.keyed_per_minute,
        "networks": networks,
    }))
}

async fn lane_health(state: &AppState) -> Health {
    Health {
        status: "ok",
        sponsor: state.config.sponsor.address(),
        network: state.config.network_passphrase.clone(),
        funds: state.sponsor_funds().await.ok(),
        anon_per_minute: state.config.limits.anon_per_minute,
        keyed_per_minute: state.config.limits.keyed_per_minute,
    }
}

/// Who is calling, and may they.
///
/// A key is only an identity, never an authorisation: an unkeyed caller is
/// served too, just from a smaller budget.
fn caller_of(state: &AppState, headers: &HeaderMap, peer: Option<IpAddr>) -> Caller {
    let presented = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        });
    if let Some(token) = presented {
        match state.config.keys.verify(token, now_secs()) {
            // The id, never the token: this ends up in rate-limit state and in
            // logs.
            Ok(key) => return Caller::Key(key.id),
            Err(reason) => {
                tracing::debug!(?reason, "ignoring an unusable key");
            }
        }
    }
    // Believing a forwarded address without a proxy in front lets a caller
    // choose their own bucket, which is the same as having no limit at all.
    let forwarded = if state.config.trust_proxy {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    } else {
        None
    };
    Caller::Anonymous(crate::limits::bucket_ip(
        forwarded.or(peer).unwrap_or(IpAddr::from([0, 0, 0, 0])),
    ))
}

fn enforce(state: &AppState, decision: Decision) -> Result<(), ApiError> {
    match decision {
        Decision::Allow => Ok(()),
        Decision::Deny {
            retry_after,
            reason,
        } => {
            state.metrics.rate_limited();
            Err(ApiError::rate_limited(reason, retry_after))
        }
    }
}

/// An asset code is not an identity on Stellar: the issuer is. Every fee token
/// is checked against the allowlist by its full `CODE:ISSUER` form.
fn check_token(
    cfg: &crate::config::Config,
    asset: &reserve_core::asset::Asset,
) -> Result<(), ApiError> {
    if cfg.token_allowlist.allows(asset) {
        return Ok(());
    }
    Err(ApiError::bad_request(
        "token_not_allowed",
        format!(
            "{} is not accepted as a fee token; see GET /v1/tokens",
            asset.canonical()
        ),
    ))
}

async fn tokens(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cfg = &state.config;
    let accepted = cfg.token_allowlist.entries();
    // Annotate with what is known about each issuer, so a client can show a
    // name and a domain rather than a raw address.
    let known: Vec<serde_json::Value> = reserve_core::tokens::known_tokens(&cfg.network_passphrase)
        .iter()
        .filter(|t| {
            accepted
                .iter()
                .any(|a| a == &t.canonical() || (t.issuer.is_empty() && a == "XLM"))
        })
        .map(|t| {
            serde_json::json!({
                "asset": t.canonical(),
                "code": t.code,
                "issuer": t.issuer,
                "domain": t.domain,
                "slippage_bps": t.slippage_bps,
                "note": t.note,
            })
        })
        .collect();
    Json(serde_json::json!({ "tokens": accepted, "known": known }))
}

#[derive(Deserialize)]
struct ChallengeRequest {
    address: String,
}

#[derive(Serialize)]
struct ChallengeResponse {
    /// Unsigned transaction to sign with the wallet holding `address`. It has
    /// sequence number zero, so the network can never accept it.
    xdr: String,
    network_passphrase: String,
    expires_in: u64,
}

/// Hand out something to sign that proves an address is yours.
async fn challenge(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ChallengeRequest>,
) -> ApiResult<ChallengeResponse> {
    enforce(
        &state,
        state
            .limits
            .check(&caller_of(&state, &headers, Some(peer.ip()))),
    )?;
    let xdr = crate::auth::challenge(&req.address, &state.config.quote_key, now_secs())
        .map_err(|_| ApiError::bad_request("invalid_address", "not a Stellar address"))?;
    Ok(Json(ChallengeResponse {
        xdr,
        network_passphrase: state.config.network_passphrase.clone(),
        expires_in: 300,
    }))
}

#[derive(Deserialize)]
struct IssueKeyRequest {
    signed_xdr: String,
}

#[derive(Serialize)]
struct IssueKeyResponse {
    key: String,
    /// The address the key belongs to. Signing again returns the same key.
    id: String,
    expires_at: u64,
    requests_per_minute: u32,
}

/// Turn a signed challenge into a key.
///
/// The key is a signed statement, not a row: nothing is stored, and signing
/// the challenge again returns the same key. There is no account to recover.
async fn issue_key(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<IssueKeyRequest>,
) -> ApiResult<IssueKeyResponse> {
    enforce(
        &state,
        state
            .limits
            .check(&caller_of(&state, &headers, Some(peer.ip()))),
    )?;
    let issuer = state.config.key_issuer.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "issuing_disabled",
            "this deployment does not hand out keys",
        )
    })?;

    let address = crate::auth::verify(
        &req.signed_xdr,
        &state.config.quote_key,
        &state.config.network_passphrase,
        now_secs(),
    )
    .map_err(|e| match e {
        crate::auth::Invalid::Expired => {
            ApiError::bad_request("challenge_expired", "ask for a new challenge")
        }
        crate::auth::Invalid::BadSignature => {
            ApiError::bad_request("bad_signature", "that account did not sign this")
        }
        crate::auth::Invalid::Malformed(why) => ApiError::bad_request("invalid_challenge", why),
    })?;

    // A year, and deterministic: the same address signing again is handed the
    // same key rather than a second one.
    let expires_at = (now_secs() / 31_536_000 + 1) * 31_536_000;
    let key = reserve_core::asset::account_id(&address)
        .map(|_| crate::apikey::IssuedKey {
            id: address.clone(),
            network: Some(state.config.network_passphrase.clone()),
            expires_at: Some(expires_at),
        })
        .map_err(ApiError::from)?;

    Ok(Json(IssueKeyResponse {
        key: issuer
            .mint(&key)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "issuing", e))?,
        id: address,
        expires_at,
        requests_per_minute: state.config.limits.keyed_per_minute,
    }))
}

#[derive(Deserialize)]
struct QuoteRequest {
    source: String,
    /// Canonical asset the user pays us in. Ignored in bootstrap mode.
    #[serde(default = "native")]
    fee_token: String,
    ops: Vec<UserOp>,
}

fn native() -> String {
    "native".to_string()
}

#[derive(Serialize)]
struct QuoteResponse {
    quote: String,
    mode: Mode,
    /// XLM the sponsor receives, margin included.
    charge_stroops: i64,
    /// Cap on what leaves the user's balance in `fee_token`.
    send_max_stroops: i64,
    /// XLM this locks up in reserves on our side.
    reserve_stroops: i64,
    /// Head-room applied for this asset. `send_max_stroops` is the worst case;
    /// strict-receive spends only what the market asks.
    slippage_bps: u32,
    /// True when this transaction also creates the account.
    creates_account: bool,
    expires_at_ledger: u32,
}

async fn quote(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<QuoteRequest>,
) -> ApiResult<QuoteResponse> {
    let cfg = &state.config;
    // Quoting is the expensive endpoint: it does path finding against Horizon
    // and costs the caller nothing.
    enforce(
        &state,
        state
            .limits
            .check(&caller_of(&state, &headers, Some(peer.ip()))),
    )?;
    let fee_token = parse_asset(&req.fee_token)?;
    check_token(cfg, &fee_token)?;

    let account = state.horizon.account(&req.source).await?;
    let plan = Plan::new(req.ops.clone(), account.is_some())?;
    let params = state.network_params().await?;

    let cost = pricing::price(
        &state.horizon,
        &plan,
        &fee_token,
        &req.source,
        &params,
        &cfg.pricing,
        &cfg.network_passphrase,
    )
    .await?;

    // A well-formed balance id is not a balance. Checking here keeps us from
    // quoting a transaction that will fail; the same check runs at submit,
    // which is what actually saves the sponsor's sequence number.
    reserve_core::claim::ensure_claims(
        &state.horizon,
        &req.source,
        &plan.ops,
        plan.mode,
        now_secs() as i64,
        reserve_core::claim::need_for(plan.mode, &fee_token.canonical(), cost.send_max_stroops)
            .as_ref(),
    )
    .await?;

    // Sponsored: the user sources the transaction. Bootstrap: a leased lane
    // does, so two concurrent quotes cannot be handed the same sequence.
    let (channel, sequence) = match plan.mode {
        Mode::Sponsored => {
            let sequence = account.and_then(|a| a.sequence_i64()).ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "no_sequence",
                    "cannot read sequence",
                )
            })? + 1;
            (String::new(), sequence)
        }
        Mode::Bootstrap => {
            let (address, sequence) = state
                .channels
                .lease(&state.horizon, Duration::from_secs(TX_TIMEOUT_SECS))
                .await?;
            let channel = if address == cfg.sponsor.address() {
                String::new()
            } else {
                address
            };
            (channel, sequence)
        }
    };

    let now = now_secs();
    let q = Quote {
        network: cfg.network_passphrase.clone(),
        mode: plan.mode,
        source: req.source.clone(),
        sponsor: cfg.sponsor.address(),
        channel,
        ops: req.ops,
        fee_token: fee_token.canonical(),
        charge_stroops: cost.charge_stroops,
        send_max_stroops: cost.send_max_stroops,
        path: cost.path.iter().map(|a| a.canonical()).collect(),
        reserve_stroops: cost.reserve_stroops,
        sequence,
        inner_fee_stroops: params.base_fee_stroops as u32 * plan.op_count(),
        min_time: 0,
        max_time: now + TX_TIMEOUT_SECS,
        expires_at_ledger: params.latest_ledger + cfg.pricing.validity_ledgers,
    };

    Ok(Json(QuoteResponse {
        quote: {
            state.metrics.quoted();
            q.seal(&cfg.quote_key)?
        },
        mode: q.mode,
        charge_stroops: q.charge_stroops,
        send_max_stroops: q.send_max_stroops,
        reserve_stroops: q.reserve_stroops,
        slippage_bps: cost.slippage_bps,
        creates_account: q.mode == Mode::Bootstrap,
        expires_at_ledger: q.expires_at_ledger,
    }))
}

#[derive(Deserialize)]
struct BuildRequest {
    quote: String,
}

#[derive(Serialize)]
struct BuildResponse {
    /// Base64 XDR of the *unsigned* transaction envelope — the shape wallets
    /// already know how to sign.
    xdr: String,
    network_passphrase: String,
    /// Accounts whose signature the network will require.
    signers: Vec<String>,
}

async fn build(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<BuildRequest>,
) -> ApiResult<BuildResponse> {
    enforce(
        &state,
        state
            .limits
            .check(&caller_of(&state, &headers, Some(peer.ip()))),
    )?;
    let q = Quote::open(&req.quote, &state.config.quote_key)?;
    let tx = build_inner(&q)?;
    let envelope = stellar_xdr::TransactionEnvelope::Tx(stellar_xdr::TransactionV1Envelope {
        tx,
        signatures: vec![].try_into().expect("empty signatures"),
    });
    let xdr = {
        use stellar_xdr::{Limits, WriteXdr};
        envelope
            .to_xdr_base64(Limits::none())
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "xdr", e.to_string()))?
    };
    // The user always signs. In bootstrap the sponsor co-signs as the source,
    // which happens server-side at submit time.
    Ok(Json(BuildResponse {
        xdr,
        network_passphrase: q.network.clone(),
        signers: vec![q.source.clone()],
    }))
}

#[derive(Deserialize)]
struct SubmitRequest {
    quote: String,
    signed_xdr: String,
}

#[derive(Serialize)]
struct SubmitResponse {
    hash: String,
    ledger: Option<u32>,
}

async fn submit(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SubmitRequest>,
) -> ApiResult<SubmitResponse> {
    let cfg = &state.config;
    enforce(
        &state,
        state
            .limits
            .check(&caller_of(&state, &headers, Some(peer.ip()))),
    )?;
    let q = Quote::open(&req.quote, &cfg.quote_key)?;
    let params = state.network_params().await?;

    // Last look before we sign. A quote can outlive the balance it named:
    // someone else claims it, a predicate has not opened, or Horizon 404s.
    // Submitting anyway is how an unfunded attacker burns a sequence number.
    // Horizon is not the ledger, so a claim can still vanish between this
    // GET and apply; we cannot close that window. We can free the lane now
    // when we already know this quote will never land.
    if let Err(e) = reserve_core::claim::ensure_claims(
        &state.horizon,
        &q.source,
        &q.ops,
        q.mode,
        now_secs() as i64,
        reserve_core::claim::need_for(q.mode, &q.fee_token, q.send_max_stroops).as_ref(),
    )
    .await
    {
        if bootstrap_lane_spent(&e) {
            release_bootstrap(&state, &q).await;
        }
        return Err(e.into());
    }

    // Same moment as the claim check: if the frozen hops cannot still buy
    // `charge` inside `sendMax`, a submit would pay the fee-bump and collect
    // nothing. Refuse before we sign.
    if let Err(e) = reserve_core::path::ensure_quoted_path(
        &state.horizon,
        &q.source,
        &q.fee_token,
        q.charge_stroops,
        q.send_max_stroops,
        &q.path,
    )
    .await
    {
        if bootstrap_lane_spent(&e) {
            release_bootstrap(&state, &q).await;
        }
        return Err(e.into());
    }

    // The gate: rebuild from the quote and demand an exact XDR match.
    let inner = match check_signed_inner(&req.signed_xdr, &q, params.latest_ledger) {
        Ok(inner) => inner,
        Err(e) => {
            if bootstrap_lane_spent(&e) {
                release_bootstrap(&state, &q).await;
            }
            return Err(e.into());
        }
    };

    let inner_fee = inner.tx.fee;
    {
        let funds = state.sponsor_funds().await.map_err(ApiError::from)?;
        let ops = (inner.tx.operations.len() as i64).saturating_add(1);
        let bump = params.base_fee_stroops.saturating_mul(ops);
        let needed = q.reserve_stroops.saturating_add(bump);
        if funds.spendable_stroops < needed {
            if q.mode == Mode::Bootstrap {
                release_bootstrap(&state, &q).await;
            }
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "sponsor_float",
                format!(
                    "sponsor has {} spendable stroops, need {needed} before this fee-bump",
                    funds.spendable_stroops
                ),
            ));
        }
    }
    let envelope = match q.mode {
        // The sponsor pays via a fee bump; the user's transaction is untouched.
        Mode::Sponsored => cfg
            .sponsor
            .wrap_fee_bump(inner, params.base_fee_stroops, &q.network)?,
        Mode::Bootstrap => sign_bootstrap(&state, &q, inner, params.base_fee_stroops)?,
    };

    if let stellar_xdr::TransactionEnvelope::TxFeeBump(fb) = &envelope {
        tracing::info!(
            inner_fee = inner_fee,
            fee_bump_fee = fb.tx.fee,
            "submitting"
        );
    }
    let xdr = {
        use stellar_xdr::{Limits, WriteXdr};
        envelope
            .to_xdr_base64(Limits::none())
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "xdr", e.to_string()))?
    };

    let submitted = state.horizon.submit(&xdr).await.map_err(ApiError::from);
    let res = match submitted {
        Ok(res) => {
            state.metrics.submitted();
            res
        }
        Err(e) => {
            // The fee bump was charged whether or not the inner transaction
            // applied, so failures are worth counting even though they are too
            // small to be worth blocking.
            state.metrics.failed();
            release_bootstrap(&state, &q).await;
            return Err(e);
        }
    };
    release_bootstrap(&state, &q).await;

    Ok(Json(SubmitResponse {
        hash: res.hash,
        ledger: res.ledger,
    }))
}

/// Where a transaction ended up.
///
/// Answered from the chain on every call. Keeping our own copy of transaction
/// state would mean maintaining a second, worse ledger.
async fn status(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
) -> ApiResult<serde_json::Value> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request(
            "invalid_hash",
            "not a transaction hash",
        ));
    }

    match state.horizon.transaction(&hash).await? {
        Some(tx) => Ok(Json(serde_json::json!({
            "hash": hash,
            "status": if tx.successful { "success" } else { "failed" },
            "ledger": tx.ledger,
        }))),
        None => Ok(Json(
            serde_json::json!({ "hash": hash, "status": "not_found" }),
        )),
    }
}

/// Prometheus text format. Counters for what the service did, and the sponsor
/// balance, which is the number worth alerting on: in normal operation it only
/// rises, so a fall means a bug, a broken market, or abuse.
async fn metrics(State(state): State<Arc<AppState>>) -> String {
    let counts = state.metrics.snapshot();
    let mut out = String::new();
    for (name, help, value) in [
        ("reserve_quotes_total", "Quotes issued", counts.quotes),
        (
            "reserve_submissions_total",
            "Transactions submitted",
            counts.submissions,
        ),
        (
            "reserve_submission_failures_total",
            "Submissions the network rejected",
            counts.failures,
        ),
        (
            "reserve_rate_limited_total",
            "Requests refused by a limit",
            counts.rate_limited,
        ),
    ] {
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}\n"
        ));
    }
    if let Ok(funds) = state.sponsor_funds().await {
        for (name, help, value) in [
            (
                "reserve_sponsor_spendable_stroops",
                "Sponsor XLM that is not locked in reserves",
                funds.spendable_stroops,
            ),
            (
                "reserve_sponsor_locked_stroops",
                "Sponsor XLM immobilised as other people's reserves",
                funds.locked_stroops,
            ),
        ] {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
            ));
        }
    }
    out
}

/// Channel (if distinct) then sponsor, then a fee-bump so the channel only
/// needs the minimum reserve. When the lane *is* the sponsor, the bytes stay
/// what they have always been: one co-signature, no fee-bump.
fn sign_bootstrap(
    state: &AppState,
    q: &Quote,
    inner: stellar_xdr::TransactionV1Envelope,
    base_fee_stroops: i64,
) -> Result<stellar_xdr::TransactionEnvelope, ApiError> {
    let lane = q.tx_source();
    let mut envelope = inner;
    if lane != q.sponsor {
        let channel = state.channels.signer(lane).ok_or_else(|| {
            ApiError::bad_request(
                "unknown_channel",
                "this quote's channel is not configured here",
            )
        })?;
        envelope = match channel.co_sign(envelope, &q.network)? {
            stellar_xdr::TransactionEnvelope::Tx(signed) => signed,
            _ => {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "signer",
                    "channel co-sign produced a fee bump",
                ));
            }
        };
    }
    let signed = state.config.sponsor.co_sign(envelope, &q.network)?;
    if lane == q.sponsor {
        return Ok(signed);
    }
    match signed {
        stellar_xdr::TransactionEnvelope::Tx(inner) => {
            Ok(state
                .config
                .sponsor
                .wrap_fee_bump(inner, base_fee_stroops, &q.network)?)
        }
        other => Ok(other),
    }
}

async fn release_bootstrap(state: &AppState, q: &Quote) {
    if q.mode == Mode::Bootstrap {
        state.channels.release(q.tx_source(), q.sequence).await;
    }
}

/// The quote cannot land, so holding its bootstrap lane only blocks the next
/// honest caller. A mismatch is the opposite: the user can sign again.
fn bootstrap_lane_spent(err: &reserve_core::Error) -> bool {
    matches!(
        err,
        reserve_core::Error::Unclaimable(_)
            | reserve_core::Error::QuoteExpired { .. }
            | reserve_core::Error::PathMoved(_)
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::bootstrap_lane_spent;
    use reserve_core::Error;

    #[test]
    fn an_unclaimable_or_expired_quote_frees_the_lane() {
        assert!(bootstrap_lane_spent(&Error::Unclaimable("gone".into())));
        assert!(bootstrap_lane_spent(&Error::QuoteExpired {
            expires_at_ledger: 1
        }));
        assert!(bootstrap_lane_spent(&Error::PathMoved("book moved".into())));
    }

    #[test]
    fn a_mismatch_keeps_the_lane_so_the_user_can_sign_again() {
        assert!(!bootstrap_lane_spent(&Error::Mismatch("xdr".into())));
        assert!(!bootstrap_lane_spent(&Error::QuoteSignature));
    }
}
