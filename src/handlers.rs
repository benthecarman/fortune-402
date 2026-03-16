use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::extract::State;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::error::AppError;
use crate::fortunes::random_fortune;
use crate::l402;
use crate::lnd::LndClient;
use crate::token::Token;

pub struct AppState {
    pub lnd: Mutex<LndClient>,
    pub config: Config,
}

pub async fn get_fortune(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // Check for L402 authorization
    if let Some(auth_header) = headers.get("authorization") {
        let auth_str = auth_header
            .to_str()
            .map_err(|_| AppError::L402("invalid authorization header encoding".into()))?;

        if auth_str.starts_with("L402 ") {
            let creds = l402::parse_authorization(auth_str).map_err(AppError::L402)?;

            l402::verify_l402(&state.config.root_key, &creds.token_base64, &creds.preimage)
                .map_err(AppError::L402)?;

            let fortune = random_fortune();
            tracing::info!("Fortune dispensed");
            let body = serde_json::json!({ "fortune": fortune });
            return Ok((StatusCode::OK, axum::Json(body)).into_response());
        }
    }

    // No valid auth — create invoice and return 402 challenge
    let (payment_hash, payment_request) = {
        let mut lnd = state.lnd.lock().await;
        lnd.create_invoice(
            state.config.invoice_amount_sats,
            &state.config.invoice_memo,
            state.config.invoice_expiry_secs,
        )
        .await?
    };

    let payment_hash_arr: [u8; 32] = payment_hash
        .try_into()
        .map_err(|_| AppError::Internal(anyhow::anyhow!("unexpected payment hash length from LND")))?;

    let token = Token::mint(&state.config.root_key, payment_hash_arr);
    let token_b64 = token.serialize();
    let challenge = l402::build_challenge(&token_b64, &payment_request);

    let body = serde_json::json!({
        "payment_request": payment_request,
        "amount_sats": state.config.invoice_amount_sats,
    });

    tracing::debug!("Issued L402 challenge");

    Ok((
        StatusCode::PAYMENT_REQUIRED,
        [("www-authenticate", challenge)],
        axum::Json(body),
    )
        .into_response())
}

pub async fn health() -> &'static str {
    "ok"
}
