mod config;
mod error;
mod fortunes;
mod handlers;
mod l402;
mod lnd;
mod systemd;
mod token;

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
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

    // The listener is bound and LND is reachable, so report readiness to
    // systemd (a no-op outside a Type=notify unit)
    systemd::spawn_watchdog();
    systemd::notify_ready(&format!("Serving on http://{listen_addr}"));

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Resolves when the process receives a shutdown signal: Ctrl+C (SIGINT) or,
/// on unix, SIGTERM, which is what systemd and Docker send on stop.
///
/// Tells systemd the service is stopping before returning, so the graceful
/// shutdown drains in-flight requests while systemd waits for the exit.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to create Ctrl+C shutdown signal");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to create SIGTERM shutdown signal")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutdown signal received, stopping server");
    systemd::notify_stopping();
}
