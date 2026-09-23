use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct Config {
    pub lnd_address: String,
    pub lnd_cert_path: String,
    pub lnd_macaroon_path: String,
    pub listen_addr: SocketAddr,
    pub invoice_amount_sats: i64,
    pub invoice_memo: String,
    pub invoice_expiry_secs: i64,
    pub root_key: [u8; 32],
    /// Set when `PUBLIC_URL` is, which enables `/x402`.
    pub x402: Option<X402Config>,
}

pub struct X402Config {
    /// Public URL clients reach this server at, without a trailing slash.
    /// Paid requests are bound to it, so it must match what clients use.
    pub public_url: String,
    pub replay_db_path: PathBuf,
    pub clock_skew_secs: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let lnd_address =
            std::env::var("LND_ADDRESS").unwrap_or_else(|_| "https://127.0.0.1:10009".to_string());
        let lnd_cert_path = std::env::var("LND_CERT_PATH").context("LND_CERT_PATH must be set")?;
        let lnd_macaroon_path =
            std::env::var("LND_MACAROON_PATH").context("LND_MACAROON_PATH must be set")?;
        let listen_addr = listen_addr_from_env()?;
        let invoice_amount_sats: i64 = std::env::var("INVOICE_AMOUNT_SATS")
            .unwrap_or_else(|_| "1".to_string())
            .parse()
            .context("invalid INVOICE_AMOUNT_SATS")?;
        let invoice_memo =
            std::env::var("INVOICE_MEMO").unwrap_or_else(|_| "Fortune cookie".to_string());
        let invoice_expiry_secs: i64 = std::env::var("INVOICE_EXPIRY_SECS")
            .unwrap_or_else(|_| "300".to_string())
            .parse()
            .context("invalid INVOICE_EXPIRY_SECS")?;

        let root_key = match std::env::var("L402_ROOT_KEY") {
            Ok(hex_str) => {
                let bytes = hex::decode(&hex_str).context("L402_ROOT_KEY must be valid hex")?;
                let key: [u8; 32] = bytes.try_into().map_err(|_| {
                    anyhow::anyhow!("L402_ROOT_KEY must be 32 bytes (64 hex chars)")
                })?;
                key
            }
            Err(_) => {
                let key: [u8; 32] = rand::random();
                tracing::warn!(
                    "L402_ROOT_KEY not set, generated random key: {}. Macaroons will not survive restarts.",
                    hex::encode(key)
                );
                key
            }
        };

        let x402 = match std::env::var("PUBLIC_URL") {
            Ok(public_url) => Some(X402Config {
                public_url: parse_public_url(&public_url).context("invalid PUBLIC_URL")?,
                replay_db_path: std::env::var("REPLAY_DB_PATH")
                    .unwrap_or_else(|_| "fortune-402.db".to_string())
                    .into(),
                clock_skew_secs: std::env::var("X402_CLOCK_SKEW_SECS")
                    .unwrap_or_else(|_| "60".to_string())
                    .parse()
                    .context("invalid X402_CLOCK_SKEW_SECS")?,
            }),
            Err(_) => None,
        };

        Ok(Config {
            lnd_address,
            lnd_cert_path,
            lnd_macaroon_path,
            listen_addr,
            invoice_amount_sats,
            invoice_memo,
            invoice_expiry_secs,
            root_key,
            x402,
        })
    }
}

/// Reads the HTTP listen address from `LISTEN_ADDR`, defaulting to
/// `0.0.0.0:3402`.
///
/// Separate from [`Config::from_env`] so the `health-check` subcommand can
/// find the server without the LND credentials the rest of the config needs.
pub fn listen_addr_from_env() -> Result<SocketAddr> {
    std::env::var("LISTEN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3402".to_string())
        .parse()
        .context("invalid LISTEN_ADDR")
}

/// Validates an absolute `http`/`https` URL with no user info, query or
/// fragment, and strips any trailing slash. A path is kept as a prefix for
/// servers behind a reverse proxy that mounts them below the root.
fn parse_public_url(url: &str) -> Result<String> {
    anyhow::ensure!(url.is_ascii(), "must be ASCII");
    anyhow::ensure!(
        !url.contains(['?', '#']),
        "must not have a query or fragment"
    );
    let uri: http::Uri = url.parse()?;
    anyhow::ensure!(
        matches!(uri.scheme_str(), Some("http" | "https")),
        "must be an absolute http or https URL"
    );
    let authority = uri.authority().context("must have a host")?;
    anyhow::ensure!(
        !authority.as_str().contains('@'),
        "must not contain user info"
    );
    Ok(url.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url() {
        assert_eq!(
            parse_public_url("https://fortune.example.com/").unwrap(),
            "https://fortune.example.com"
        );
        assert_eq!(
            parse_public_url("http://127.0.0.1:3402/prefix").unwrap(),
            "http://127.0.0.1:3402/prefix"
        );
        for bad in [
            "fortune.example.com",
            "ftp://fortune.example.com",
            "https://user@fortune.example.com",
            "https://fortune.example.com/?a=b",
            "https://fortune.example.com/#x",
            "https://fortüne.example.com",
        ] {
            assert!(parse_public_url(bad).is_err(), "{bad}");
        }
    }
}
