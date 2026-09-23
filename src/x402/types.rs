//! x402 version 2 wire types and their HTTP header encoding.

use base64::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const X402_VERSION: u32 = 2;

/// Server → client: base64 `PaymentRequired`, sent with a 402.
pub const PAYMENT_REQUIRED: &str = "payment-required";
/// Client → server: base64 `PaymentPayload` on the paid retry.
pub const PAYMENT_SIGNATURE: &str = "payment-signature";
/// Server → client: base64 `SettlementResponse`.
pub const PAYMENT_RESPONSE: &str = "payment-response";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    pub x402_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub resource: ResourceInfo,
    pub accepts: Vec<PaymentRequirements>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    pub url: String,
    pub description: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    pub scheme: String,
    pub network: String,
    pub amount: String,
    pub asset: String,
    pub pay_to: String,
    pub max_timeout_seconds: u64,
    #[serde(default)]
    pub extra: Map<String, Value>,
}

/// Only the fields we act on; `resource` and anything else the client echoes
/// is ignored because the server derives what it sells from its own config.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    pub x402_version: u32,
    pub accepted: PaymentRequirements,
    pub payload: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    pub transaction: String,
    pub network: String,
}

pub fn encode_header<T: Serialize>(value: &T) -> String {
    BASE64_STANDARD.encode(serde_json::to_vec(value).expect("x402 types serialize"))
}

pub fn decode_header<T: DeserializeOwned>(value: &str) -> Result<T, String> {
    let json = BASE64_STANDARD
        .decode(value)
        .map_err(|e| format!("base64 decode error: {e}"))?;
    serde_json::from_slice(&json).map_err(|e| format!("invalid JSON: {e}"))
}
