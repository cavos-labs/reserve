//! End-to-end proof on Stellar testnet: an account that never holds a single
//! stroop of XLM gets created, opens a trustline, and then pays — funding the
//! whole thing with its own token.
//!
//! Run with the API already listening:
//!   RESERVE_SPONSOR_SECRET=S... RESERVE_URL=http://127.0.0.1:8080 \
//!     cargo run -p reserve-e2e

mod chain;

use anyhow::{anyhow, bail, Context, Result};
use reserve_horizon::{Horizon, HorizonApi};
use reserve_signer::Sponsor;
use serde_json::json;
use stellar_xdr as xdr;
use xdr::{Limits, WriteXdr};

use chain::{asset, friendbot, now, op, random_keypair, send, TESTNET_HORIZON};

const TOKEN: &str = "TEST";

struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    async fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let res = self
            .http
            .post(format!("{}{}", self.base, path))
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            bail!("{path} -> {status}: {text}");
        }
        Ok(serde_json::from_str(&text)?)
    }
}

/// Sign an unsigned transaction envelope as `keypair`.
fn sign_xdr(unsigned_xdr: &str, keypair: &Sponsor) -> Result<String> {
    let tx = match xdr::TransactionEnvelope::from_xdr_base64(unsigned_xdr, Limits::none())? {
        xdr::TransactionEnvelope::Tx(v1) => v1.tx,
        _ => bail!("expected a v1 envelope"),
    };
    let envelope = keypair.sign_transaction(tx, reserve_signer::TESTNET)?;
    Ok(envelope.to_xdr_base64(Limits::none())?)
}

async fn run_flow(
    api: &Api,
    horizon: &Horizon,
    user: &Sponsor,
    fee_token: &str,
    ops: serde_json::Value,
) -> Result<String> {
    let quote = api
        .post(
            "/v1/quote",
            json!({ "source": user.address(), "fee_token": fee_token, "ops": ops }),
        )
        .await?;
    println!("  quote: {}", serde_json::to_string(&quote)?);
    let token = quote["quote"].as_str().context("quote token")?;

    let built = api.post("/v1/build", json!({ "quote": token })).await?;
    let unsigned = built["xdr"].as_str().context("xdr")?;
    let signed = sign_xdr(unsigned, user)?;

    let submitted = api
        .post(
            "/v1/submit",
            json!({ "quote": token, "signed_xdr": signed }),
        )
        .await?;
    let hash = submitted["hash"].as_str().context("hash")?.to_string();
    println!("  submitted: {hash}");
    let _ = horizon;
    Ok(hash)
}

/// Balance of a classic credit asset, which is the same trustline the Stellar
/// Asset Contract moves.
async fn token_balance(horizon: &Horizon, address: &str, code: &str, issuer: &str) -> Result<i64> {
    let account = horizon
        .account(address)
        .await?
        .ok_or_else(|| anyhow!("{address} does not exist"))?;
    let balance = account
        .balances
        .iter()
        .find(|b| {
            b.asset_code.as_deref() == Some(code) && b.asset_issuer.as_deref() == Some(issuer)
        })
        .map(|b| b.balance.clone())
        .unwrap_or_else(|| "0".into());
    reserve_horizon::parse_amount_stroops(&balance).context("token balance")
}

/// The one claimable balance waiting for `address`.
async fn claimable_balance_id(horizon: &Horizon, address: &str) -> Result<String> {
    let url = format!(
        "https://horizon-testnet.stellar.org/claimable_balances?claimant={address}&limit=1"
    );
    let body: serde_json::Value = reqwest::get(&url).await?.json().await?;
    let id = body["_embedded"]["records"][0]["id"]
        .as_str()
        .context("no claimable balance for this address")?;
    let _ = horizon;
    Ok(id.to_string())
}

async fn native_balance(horizon: &Horizon, address: &str) -> Result<i64> {
    let account = horizon
        .account(address)
        .await?
        .ok_or_else(|| anyhow!("{address} does not exist"))?;
    account.native_balance_stroops().context("native balance")
}

#[tokio::main]
async fn main() -> Result<()> {
    let base = std::env::var("RESERVE_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let secret = std::env::var("RESERVE_SPONSOR_SECRET")
        .context("RESERVE_SPONSOR_SECRET must match the running service")?;
    let sponsor = Sponsor::from_strkey(secret.trim())?;
    let horizon = Horizon::new(TESTNET_HORIZON);
    let api = Api {
        base,
        http: reqwest::Client::new(),
    };

    println!("sponsor {}", sponsor.address());
    if horizon.account(&sponsor.address()).await?.is_none() {
        friendbot(&sponsor.address()).await?;
        println!("  funded by friendbot");
    }

    // --- fixture: an issuer and a market maker, so the fee token has a real
    // --- route to XLM on the SDEX.
    let issuer = random_keypair();
    let maker = random_keypair();
    friendbot(&issuer.address()).await?;
    friendbot(&maker.address()).await?;
    println!("issuer {}\nmaker  {}", issuer.address(), maker.address());

    let token = asset(TOKEN, &issuer.address())?;
    let change_trust_line = match token.clone() {
        xdr::Asset::CreditAlphanum4(a) => xdr::ChangeTrustAsset::CreditAlphanum4(a),
        _ => bail!("expected alphanum4"),
    };
    send(
        &horizon,
        &maker,
        vec![op(xdr::OperationBody::ChangeTrust(xdr::ChangeTrustOp {
            line: change_trust_line.clone(),
            limit: i64::MAX,
        }))],
        &[],
    )
    .await?;
    send(
        &horizon,
        &issuer,
        vec![op(xdr::OperationBody::Payment(xdr::PaymentOp {
            destination: maker.muxed_account(),
            asset: token.clone(),
            amount: 100_000 * 10_000_000,
        }))],
        &[],
    )
    .await?;
    // Sell XLM for TEST: this is the offer a fee payment consumes.
    send(
        &horizon,
        &maker,
        vec![op(xdr::OperationBody::ManageSellOffer(
            xdr::ManageSellOfferOp {
                selling: xdr::Asset::Native,
                buying: token.clone(),
                amount: 5_000 * 10_000_000,
                price: xdr::Price { n: 1, d: 2 },
                offer_id: 0,
            },
        ))],
        &[],
    )
    .await?;
    println!("market ready: 5000 XLM offered at 0.5 TEST/XLM");

    // --- 1. someone sends money to an address with no account behind it. A
    // --- claimable balance is how Stellar carries that.
    let user = random_keypair();
    println!("user {}", user.address());
    let sponsor_before = native_balance(&horizon, &sponsor.address()).await?;
    send(
        &horizon,
        &issuer,
        vec![op(xdr::OperationBody::CreateClaimableBalance(
            xdr::CreateClaimableBalanceOp {
                asset: token.clone(),
                amount: 20 * 10_000_000,
                claimants: vec![xdr::Claimant::ClaimantTypeV0(xdr::ClaimantV0 {
                    destination: xdr::AccountId(xdr::PublicKey::PublicKeyTypeEd25519(
                        xdr::Uint256(user.public_key_bytes()),
                    )),
                    predicate: xdr::ClaimPredicate::Unconditional,
                })]
                .try_into()
                .unwrap(),
            },
        ))],
        &[],
    )
    .await?;
    let balance_id = claimable_balance_id(&horizon, &user.address()).await?;
    println!(
        "  20 {TOKEN} waiting as claimable balance {}",
        &balance_id[..16]
    );

    // The account is created, gets its trustline, claims the money, and pays
    // for all of it out of what it just claimed — in one transaction.
    run_flow(
        &api,
        &horizon,
        &user,
        &format!("{TOKEN}:{}", issuer.address()),
        json!([
            { "type": "create_account", "destination": user.address() },
            { "type": "change_trust", "asset": format!("{TOKEN}:{}", issuer.address()) },
            { "type": "claim_balance", "balance_id": balance_id },
        ]),
    )
    .await?;

    let account = horizon
        .account(&user.address())
        .await?
        .ok_or_else(|| anyhow!("user account was not created"))?;
    let xlm = account.native_balance_stroops().context("native balance")?;
    println!(
        "  account created: {} XLM, num_sponsored={}, subentries={}",
        reserve_horizon::format_stroops(xlm),
        account.num_sponsored,
        account.subentry_count
    );
    if xlm != 0 {
        bail!("expected a zero-XLM account, got {xlm} stroops");
    }
    if account.num_sponsored == 0 {
        bail!("reserves were not sponsored");
    }

    let claimed = token_balance(&horizon, &user.address(), TOKEN, &issuer.address()).await?;
    println!(
        "  claimed {} {TOKEN}, so the reserves and fees came out of the money that arrived",
        reserve_horizon::format_stroops(claimed)
    );
    if claimed >= 20 * 10_000_000 {
        bail!("the account did not pay for its own reserves");
    }

    // --- 2. top the user up, still without a single stroop of XLM.
    send(
        &horizon,
        &issuer,
        vec![op(xdr::OperationBody::Payment(xdr::PaymentOp {
            destination: user.muxed_account(),
            asset: token.clone(),
            amount: 100 * 10_000_000,
        }))],
        &[],
    )
    .await?;
    println!("user topped up with 100 {TOKEN}");

    // --- 3. a payment that pays for itself in TEST.
    run_flow(
        &api,
        &horizon,
        &user,
        &format!("{TOKEN}:{}", issuer.address()),
        json!([{
            "type": "payment",
            "destination": maker.address(),
            "asset": format!("{TOKEN}:{}", issuer.address()),
            "amount": "1.0000000"
        }]),
    )
    .await?;

    let xlm_after = native_balance(&horizon, &user.address()).await?;
    let sponsor_after = native_balance(&horizon, &sponsor.address()).await?;
    println!(
        "  user XLM after: {} | sponsor delta: {} stroops (reserves locked included)",
        reserve_horizon::format_stroops(xlm_after),
        sponsor_after - sponsor_before
    );
    if xlm_after != 0 {
        bail!("user ended up holding XLM: {xlm_after} stroops");
    }

    // --- 4. negative: a stale quote must be refused.
    let quote = api
        .post(
            "/v1/quote",
            json!({
                "source": user.address(),
                "fee_token": format!("{TOKEN}:{}", issuer.address()),
                "ops": [{
                    "type": "payment",
                    "destination": maker.address(),
                    "asset": format!("{TOKEN}:{}", issuer.address()),
                    "amount": "1.0000000"
                }]
            }),
        )
        .await?;
    let token_str = quote["quote"].as_str().context("quote")?;
    let built = api.post("/v1/build", json!({ "quote": token_str })).await?;
    let tampered = {
        // Lower what we get paid, keeping everything else identical.
        let mut tx = match xdr::TransactionEnvelope::from_xdr_base64(
            built["xdr"].as_str().context("xdr")?,
            Limits::none(),
        )? {
            xdr::TransactionEnvelope::Tx(v1) => v1.tx,
            _ => bail!("expected a v1 envelope"),
        };
        let mut ops = tx.operations.to_vec();
        for o in ops.iter_mut() {
            if let xdr::OperationBody::PathPaymentStrictReceive(p) = &mut o.body {
                p.dest_amount = 1;
            }
        }
        tx.operations = ops.try_into().map_err(|_| anyhow!("ops"))?;
        let envelope = xdr::TransactionEnvelope::Tx(xdr::TransactionV1Envelope {
            tx,
            signatures: vec![].try_into().unwrap(),
        });
        sign_xdr(&envelope.to_xdr_base64(Limits::none())?, &user)?
    };
    match api
        .post(
            "/v1/submit",
            json!({ "quote": token_str, "signed_xdr": tampered }),
        )
        .await
    {
        Ok(v) => bail!("tampered transaction was accepted: {v}"),
        Err(e) => println!("  tampered submit rejected as expected: {e}"),
    }

    let _ = now();
    println!("\nOK: an account with 0 XLM created itself, opened a trustline, and paid — all in {TOKEN}.");
    Ok(())
}
