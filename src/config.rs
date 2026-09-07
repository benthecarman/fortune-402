use std::net::SocketAddr;

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

        Ok(Config {
            lnd_address,
            lnd_cert_path,
            lnd_macaroon_path,
            listen_addr,
            invoice_amount_sats,
            invoice_memo,
            invoice_expiry_secs,
            root_key,
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
