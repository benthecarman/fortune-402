mod config;
mod error;
mod fortunes;
mod handlers;
mod l402;
mod lnd;
mod systemd;
mod token;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::routing::get;
use axum::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
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

    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("health-check") => return health_probe(config::listen_addr_from_env()?).await,
        Some(other) => anyhow::bail!("unknown command {other:?}, expected `health-check`"),
    }

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

/// Probes the local server's `/health` route and returns an error if it does
/// not answer with a success status. Backs the `health-check` subcommand used
/// by the Docker `HEALTHCHECK` and similar liveness checks.
///
/// `listen_addr` is the address the server listens on; a wildcard address maps
/// to loopback. The request is a plain HTTP/1.1 GET over a TCP socket so the
/// probe needs no HTTP client dependency.
async fn health_probe(listen_addr: SocketAddr) -> anyhow::Result<()> {
    let mut addr = listen_addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(match addr.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        });
    }
    let url = format!("http://{addr}/health");

    let request = async {
        let mut stream = TcpStream::connect(addr).await?;
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        anyhow::Ok(response)
    };
    let response = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .with_context(|| format!("{url} did not answer within 5s"))?
        .with_context(|| format!("{url} request failed"))?;

    // Status line is "HTTP/1.1 200 OK"
    let head = String::from_utf8_lossy(&response);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .and_then(|code| http::StatusCode::from_u16(code).ok())
        .with_context(|| format!("{url} returned a malformed response"))?;

    if status.is_success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{url} returned {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    /// Serves `router` on a random loopback port.
    async fn serve(router: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        addr
    }

    #[tokio::test]
    async fn health_probe_passes_for_running_server() {
        let addr = serve(Router::new().route("/health", get(handlers::health))).await;
        health_probe(addr).await.unwrap();
        // a wildcard bind is probed over loopback
        health_probe(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), addr.port()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn health_probe_fails_for_error_status() {
        let addr = serve(
            Router::new().route("/health", get(|| async { StatusCode::SERVICE_UNAVAILABLE })),
        )
        .await;
        let err = health_probe(addr).await.unwrap_err();
        assert!(err.to_string().contains("503"), "{err}");
    }

    #[tokio::test]
    async fn health_probe_fails_without_server() {
        // grab a free port and release it so nothing listens there
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(health_probe(addr).await.is_err());
    }
}
