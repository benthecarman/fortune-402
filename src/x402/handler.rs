use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bitcoin::secp256k1::PublicKey;
use tower_http::cors::{Any, CorsLayer};

use crate::error::AppError;
use crate::fortunes::random_fortune;
use crate::handlers::AppState;
use crate::replay::ReplayStore;
use crate::x402::binding;
use crate::x402::lnbtc::{self, Network, SettleError, Terms};
use crate::x402::types::{
    decode_header, encode_header, PaymentPayload, PaymentRequired, ResourceInfo,
    SettlementResponse, PAYMENT_REQUIRED, PAYMENT_RESPONSE, PAYMENT_SIGNATURE, X402_VERSION,
};

const REPLAY_PRUNE_INTERVAL: Duration = Duration::from_secs(600);

/// Creates invoices for x402 challenges.
pub trait InvoiceIssuer: Send + Sync + 'static {
    /// Returns a fresh BOLT11 invoice whose description hash is
    /// `description_hash`.
    fn issue(
        &self,
        amount_msat: u64,
        description_hash: [u8; 32],
        expiry_secs: u64,
    ) -> impl Future<Output = Result<String, AppError>> + Send;
}

impl InvoiceIssuer for Arc<AppState> {
    async fn issue(
        &self,
        amount_msat: u64,
        description_hash: [u8; 32],
        expiry_secs: u64,
    ) -> Result<String, AppError> {
        let amount_msat = i64::try_from(amount_msat).context("invoice amount too large")?;
        let expiry_secs = i64::try_from(expiry_secs).context("invoice expiry too large")?;
        self.lnd
            .lock()
            .await
            .create_invoice_with_description_hash(amount_msat, description_hash, expiry_secs)
            .await
    }
}

pub struct X402State<I> {
    pub issuer: I,
    pub network: Network,
    /// The receiving node, which must be the only party able to issue
    /// invoices under its key.
    pub pay_to: PublicKey,
    pub amount_msat: u64,
    pub max_timeout_secs: u64,
    pub clock_skew_secs: u64,
    /// Public URL of the server, without a trailing slash.
    pub public_url: String,
    pub replay: ReplayStore,
}

impl X402State<Arc<AppState>> {
    /// Builds the `/x402` state from the server config, or returns `None` if
    /// x402 is not configured or LND's network has no `lnbtc` identifier.
    pub async fn from_app(app: &Arc<AppState>) -> anyhow::Result<Option<Self>> {
        let Some(config) = &app.config.x402 else {
            tracing::info!("PUBLIC_URL not set, /x402 disabled");
            return Ok(None);
        };

        let info = app.lnd.lock().await.get_info().await?;
        let Some(network) = Network::from_lnd(&info.network) else {
            tracing::warn!(
                "LND is on {}, which x402 Lightning does not support (only mainnet, testnet3 and signet), /x402 disabled",
                info.network
            );
            return Ok(None);
        };
        let pay_to =
            PublicKey::from_str(&info.pubkey).context("LND returned an invalid node pubkey")?;

        let amount_msat = app
            .config
            .invoice_amount_sats
            .checked_mul(1000)
            .and_then(|msat| u64::try_from(msat).ok())
            .filter(|&msat| msat > 0)
            .context("INVOICE_AMOUNT_SATS must be positive for x402")?;
        let max_timeout_secs = u64::try_from(app.config.invoice_expiry_secs)
            .ok()
            .filter(|&secs| secs > 0)
            .context("INVOICE_EXPIRY_SECS must be positive for x402")?;

        let replay = ReplayStore::open(&config.replay_db_path)?;
        spawn_replay_pruner(replay.clone());

        tracing::info!(
            "x402 enabled at {}/x402 on {}",
            config.public_url,
            network.caip2()
        );
        if network == Network::Signet {
            tracing::warn!(
                "Signet is not in the x402 Lightning spec; only clients that accept {} can pay",
                network.caip2()
            );
        }
        Ok(Some(X402State {
            issuer: app.clone(),
            network,
            pay_to,
            amount_msat,
            max_timeout_secs,
            clock_skew_secs: config.clock_skew_secs,
            public_url: config.public_url.clone(),
            replay,
        }))
    }
}

fn spawn_replay_pruner(replay: ReplayStore) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REPLAY_PRUNE_INTERVAL);
        loop {
            interval.tick().await;
            match replay.prune(unix_now()) {
                Ok(0) => {}
                Ok(pruned) => tracing::debug!("Pruned {pruned} expired x402 replay keys"),
                Err(e) => tracing::warn!("Failed to prune x402 replay store: {e:#}"),
            }
        }
    });
}

pub fn router<I: InvoiceIssuer>(state: Arc<X402State<I>>) -> Router {
    // Lets browser clients read the challenge and settlement headers and
    // send the payment header
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET])
        .allow_headers([HeaderName::from_static(PAYMENT_SIGNATURE)])
        .expose_headers([
            HeaderName::from_static(PAYMENT_REQUIRED),
            HeaderName::from_static(PAYMENT_RESPONSE),
        ]);

    Router::new()
        .route("/x402", get(get_fortune::<I>))
        .layer(cors)
        .with_state(state)
}

pub async fn get_fortune<I: InvoiceIssuer>(
    State(state): State<Arc<X402State<I>>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    // What is bought is derived from the request itself, never from what the
    // client echoes back in its payment
    let path = uri.path_and_query().map_or(uri.path(), |p| p.as_str());
    let url = format!("{}{path}", state.public_url);
    let request_hash = binding::http_request_hash(
        method.as_str(),
        &url,
        &body,
        binding::BOUND_HEADERS,
        &headers,
    )
    .map_err(AppError::X402)?;
    let terms = state.terms(request_hash);

    let Some(signature) = headers.get(PAYMENT_SIGNATURE) else {
        return state
            .challenge(&terms, url, "PAYMENT-SIGNATURE header is required", None)
            .await;
    };

    let payload: PaymentPayload = signature
        .to_str()
        .map_err(|_| "invalid header encoding".to_string())
        .and_then(decode_header)
        .map_err(|e| AppError::X402(format!("malformed PAYMENT-SIGNATURE header: {e}")))?;
    if payload.x402_version != X402_VERSION {
        return Err(AppError::X402(format!(
            "unsupported x402Version {}, expected {X402_VERSION}",
            payload.x402_version
        )));
    }

    match lnbtc::settle(&state.replay, &payload, &terms, unix_now()) {
        Ok(settlement) => {
            tracing::info!("x402 fortune dispensed");
            let response = SettlementResponse {
                success: true,
                error_reason: None,
                transaction: settlement.transaction,
                network: state.network.caip2().to_string(),
            };
            let body = serde_json::json!({ "fortune": random_fortune() });
            Ok((
                StatusCode::OK,
                [(PAYMENT_RESPONSE, encode_header(&response))],
                Json(body),
            )
                .into_response())
        }
        Err(SettleError::Rejected(reason)) => {
            tracing::info!("x402 payment rejected: {reason}");
            let response = SettlementResponse {
                success: false,
                error_reason: Some(reason.to_string()),
                transaction: String::new(),
                network: state.network.caip2().to_string(),
            };
            state
                .challenge(&terms, url, reason.as_str(), Some(response))
                .await
        }
        Err(SettleError::Store(e)) => Err(AppError::Internal(e)),
    }
}

impl<I: InvoiceIssuer> X402State<I> {
    fn terms(&self, request_hash: [u8; 32]) -> Terms {
        Terms {
            network: self.network,
            pay_to: self.pay_to,
            amount_msat: self.amount_msat,
            max_timeout_secs: self.max_timeout_secs,
            request_hash,
            binding_params: binding::http_params(binding::BOUND_HEADERS),
            clock_skew_secs: self.clock_skew_secs,
        }
    }

    /// Responds 402 with a fresh invoice bound to the request, plus the
    /// reason an earlier payment failed, if any.
    async fn challenge(
        &self,
        terms: &Terms,
        url: String,
        error: &str,
        failure: Option<SettlementResponse>,
    ) -> Result<Response, AppError> {
        let invoice = self
            .issuer
            .issue(
                terms.amount_msat,
                terms.request_hash,
                terms.max_timeout_secs,
            )
            .await?;
        lnbtc::check_issued_invoice(&invoice, terms, unix_now()).map_err(|reason| {
            AppError::Internal(anyhow::anyhow!(
                "LND issued an invoice that fails x402 checks: {reason}"
            ))
        })?;

        let required = PaymentRequired {
            x402_version: X402_VERSION,
            error: Some(error.to_string()),
            resource: ResourceInfo {
                url,
                description: "A fortune cookie".to_string(),
                mime_type: "application/json".to_string(),
            },
            accepts: vec![terms.requirements(&invoice)],
        };
        tracing::debug!("Issued x402 challenge");

        let mut response = (
            StatusCode::PAYMENT_REQUIRED,
            [(PAYMENT_REQUIRED, encode_header(&required))],
            Json(&required),
        )
            .into_response();
        if let Some(failure) = failure {
            let value = HeaderValue::from_str(&encode_header(&failure))
                .expect("base64 is a valid header value");
            response.headers_mut().insert(PAYMENT_RESPONSE, value);
        }
        Ok(response)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after 1970")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use axum::body::Body;
    use axum::http::Request;
    use base64::prelude::*;
    use bitcoin::secp256k1::Secp256k1;
    use lightning_invoice::Currency;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;
    use crate::x402::lnbtc::tests::{secret_key, sign_invoice};

    const PUBLIC_URL: &str = "https://fortune.test";

    /// Signs invoices with a test key and remembers their preimages, standing
    /// in for both the receiving LND and the payer's wallet.
    #[derive(Default)]
    struct FakeIssuer {
        preimages: Mutex<HashMap<String, [u8; 32]>>,
    }

    impl InvoiceIssuer for FakeIssuer {
        async fn issue(
            &self,
            amount_msat: u64,
            description_hash: [u8; 32],
            expiry_secs: u64,
        ) -> Result<String, AppError> {
            let (invoice, preimage) = sign_invoice(
                &secret_key(),
                Currency::Bitcoin,
                amount_msat,
                description_hash,
                expiry_secs,
                unix_now(),
            );
            self.preimages
                .lock()
                .unwrap()
                .insert(invoice.clone(), preimage);
            Ok(invoice)
        }
    }

    fn app() -> (Router, Arc<X402State<FakeIssuer>>) {
        let state = Arc::new(X402State {
            issuer: FakeIssuer::default(),
            network: Network::Mainnet,
            pay_to: PublicKey::from_secret_key(&Secp256k1::new(), &secret_key()),
            amount_msat: 1000,
            max_timeout_secs: 300,
            clock_skew_secs: 60,
            public_url: PUBLIC_URL.to_string(),
            replay: ReplayStore::open(Path::new(":memory:")).unwrap(),
        });
        (router(state.clone()), state)
    }

    async fn get(app: &Router, uri: &str, signature: Option<&str>) -> Response {
        let mut request = Request::get(uri);
        if let Some(signature) = signature {
            request = request.header(PAYMENT_SIGNATURE, signature);
        }
        app.clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    fn header_json(response: &Response, name: &str) -> Value {
        let value = response.headers()[name].to_str().unwrap();
        serde_json::from_slice(&BASE64_STANDARD.decode(value).unwrap()).unwrap()
    }

    /// Requests `uri` unpaid and pays the offered invoice, returning the
    /// `PAYMENT-SIGNATURE` value.
    async fn pay(app: &Router, state: &X402State<FakeIssuer>, uri: &str) -> String {
        let response = get(app, uri, None).await;
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        let required = header_json(&response, PAYMENT_REQUIRED);
        let accepted = required["accepts"][0].clone();
        let invoice = accepted["extra"]["invoice"].as_str().unwrap();
        let preimage = state.issuer.preimages.lock().unwrap()[invoice];

        let payload = json!({
            "x402Version": 2,
            "resource": required["resource"],
            "accepted": accepted,
            "payload": { "preimage": hex::encode(preimage) },
        });
        BASE64_STANDARD.encode(payload.to_string())
    }

    #[tokio::test]
    async fn challenge_describes_request() {
        let (app, _) = app();
        let response = get(&app, "/x402?lang=en", None).await;
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);

        let required = header_json(&response, PAYMENT_REQUIRED);
        assert_eq!(required["x402Version"], 2);
        assert_eq!(
            required["resource"]["url"],
            "https://fortune.test/x402?lang=en"
        );

        let accepted = &required["accepts"][0];
        assert_eq!(accepted["network"], Network::Mainnet.caip2());
        assert_eq!(accepted["amount"], "1000");
        assert_eq!(accepted["asset"], "BTC");
        assert_eq!(accepted["maxTimeoutSeconds"], 300);
        assert_eq!(accepted["extra"]["paymentFlow"], "upfront");
        assert_eq!(accepted["extra"]["requestBindingProfile"], "http:1");
        assert_eq!(
            accepted["extra"]["requestBindingParams"],
            json!({ "headers": [] })
        );

        let expected_hash = binding::http_request_hash(
            "GET",
            "https://fortune.test/x402?lang=en",
            b"",
            &[],
            &HeaderMap::new(),
        )
        .unwrap();
        assert_eq!(accepted["extra"]["requestHash"], hex::encode(expected_hash));
    }

    #[tokio::test]
    async fn paid_request_gets_fortune_once() {
        let (app, state) = app();
        let signature = pay(&app, &state, "/x402").await;

        let response = get(&app, "/x402", Some(&signature)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let settlement = header_json(&response, PAYMENT_RESPONSE);
        assert_eq!(settlement["success"], true);
        assert_eq!(settlement["network"], Network::Mainnet.caip2());
        assert_eq!(settlement["transaction"].as_str().unwrap().len(), 64);
        assert!(settlement.get("payer").is_none());
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["fortune"].is_string());

        // replaying the same proof gets a fresh challenge instead
        let response = get(&app, "/x402", Some(&signature)).await;
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        let settlement = header_json(&response, PAYMENT_RESPONSE);
        assert_eq!(settlement["success"], false);
        assert_eq!(settlement["errorReason"], "duplicate_settlement");
        assert!(response.headers().contains_key(PAYMENT_REQUIRED));
    }

    #[tokio::test]
    async fn proof_is_bound_to_its_request() {
        let (app, state) = app();
        let signature = pay(&app, &state, "/x402?lang=en").await;

        let response = get(&app, "/x402?lang=fr", Some(&signature)).await;
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        let settlement = header_json(&response, PAYMENT_RESPONSE);
        assert_eq!(
            settlement["errorReason"],
            "invalid_exact_lnbtc_request_mismatch"
        );

        // the proof was not consumed by the failed attempt
        let response = get(&app, "/x402?lang=en", Some(&signature)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_payment_is_bad_request() {
        let (app, _) = app();
        let response = get(&app, "/x402", Some("not base64!")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let wrong_version = BASE64_STANDARD.encode(
            json!({ "x402Version": 1, "accepted": {
                "scheme": "exact", "network": "", "amount": "", "asset": "",
                "payTo": "", "maxTimeoutSeconds": 1
            }, "payload": {} })
            .to_string(),
        );
        let response = get(&app, "/x402", Some(&wrong_version)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
