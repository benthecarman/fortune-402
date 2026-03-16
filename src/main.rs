mod config;
mod error;
mod fortunes;
mod handlers;
mod l402;
mod lnd;
mod token;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::handlers::AppState;
use crate::lnd::LndClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fortune_402=info,tower_http=info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let listen_addr = config.listen_addr;

    let lnd_client = LndClient::connect(&config).await?;

    let state = Arc::new(AppState {
        lnd: tokio::sync::Mutex::new(lnd_client),
        config,
    });

    let app = Router::new()
        .route("/", get(handlers::index))
        .route("/fortune", get(handlers::get_fortune))
        .route("/health", get(handlers::health))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    tracing::info!("Listening on {listen_addr}");
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
