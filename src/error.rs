use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("LND error: {0}")]
    Lnd(#[from] fedimint_tonic_lnd::tonic::Status),

    #[error("LND connection error: {0}")]
    LndConnection(String),

    #[error("L402 auth error: {0}")]
    L402(String),

    #[error("{0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::L402(_) => StatusCode::UNAUTHORIZED,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = serde_json::json!({ "error": self.to_string() });
        (status, axum::Json(body)).into_response()
    }
}
