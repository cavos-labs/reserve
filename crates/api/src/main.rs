mod apikey;
mod auth;
mod channels;
mod config;
mod limits;
mod metrics;
mod routes;
mod state;

use std::sync::Arc;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let boot = config::Boot::from_env().context("configuration")?;
    let bind = boot.bind.clone();

    let mut lanes = Vec::new();
    for lane in boot.lanes {
        let state = Arc::new(state::AppState::new(lane.config)?);
        limits::spawn_pruner(state.clone(), std::time::Duration::from_secs(5 * 60));
        tracing::info!(
            slug = lane.slug,
            sponsor = %state.config.sponsor.address(),
            bootstrap_lanes = state.channels.len(),
            fee_tokens = %state.config.token_allowlist.entries().join(", "),
            "reserve lane"
        );
        lanes.push((lane.slug, state));
    }

    let slugs: Vec<_> = lanes.iter().map(|(s, _)| *s).collect();
    let app = routes::app(lanes);
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, networks = %slugs.join(","), "reserve up");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
