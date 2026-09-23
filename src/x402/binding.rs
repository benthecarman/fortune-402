//! Request binding for the Lightning `exact` scheme.
//!
//! The invoice's BOLT11 description hash commits to a SHA-256 digest of a
//! JCS-encoded (RFC 8785) description of the request, so a proof paid for one
//! request cannot buy another at the same price.

use axum::http::HeaderMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const HTTP_PROFILE: &str = "http:1";

/// Headers that select what `/x402` sells. None do: every request buys one
/// random fortune.
pub const BOUND_HEADERS: &[&str] = &[];

/// `requestBindingParams` for the `http:1` profile.
pub fn http_params(bound_headers: &[&str]) -> Value {
    json!({ "headers": bound_headers })
}

/// Whether `params` is a well-formed `http:1` parameter object: exactly a
/// `headers` array of lowercase field names in ascending order without
/// duplicates, not including `payment-signature`.
pub fn is_valid_http_params(params: &Value) -> bool {
    let Some(params) = params.as_object() else {
        return false;
    };
    let Some(Value::Array(headers)) = params.get("headers") else {
        return false;
    };
    if params.len() != 1 {
        return false;
    }

    let mut previous: Option<&str> = None;
    for header in headers {
        let Some(name) = header.as_str() else {
            return false;
        };
        if !is_lowercase_token(name) || name == "payment-signature" {
            return false;
        }
        if previous.is_some_and(|previous| previous >= name) {
            return false;
        }
        previous = Some(name);
    }
    true
}

/// RFC 9110 `token` with no uppercase letters.
fn is_lowercase_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}

/// Computes the `http:1` request hash.
///
/// `url` is the absolute public URL of the request, including its query.
/// `body` is the content after transfer decoding. Fails if a bound header has
/// a value outside visible ASCII.
pub fn http_request_hash(
    method: &str,
    url: &str,
    body: &[u8],
    bound_headers: &[&str],
    headers: &HeaderMap,
) -> Result<[u8; 32], String> {
    let headers = bound_headers
        .iter()
        .map(|&name| {
            let value_hash = header_value_hash(headers, name)?;
            Ok(json!({ "name": name, "valueHash": hex::encode(value_hash) }))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let binding = json!({
        "domain": format!("x402:exact:lnbtc:bolt11:{HTTP_PROFILE}"),
        "method": method,
        "url": url,
        "bodyHash": hex::encode(Sha256::digest(body)),
        "headers": headers,
    });
    let description = serde_jcs::to_vec(&binding).map_err(|e| e.to_string())?;
    Ok(Sha256::digest(description).into())
}

/// SHA-256 of `0x01 || value` for a present header, where multiple field lines
/// are trimmed and joined with ", " (RFC 9421 section 2.1), or of `0x00` for an
/// absent one.
fn header_value_hash(headers: &HeaderMap, name: &str) -> Result<[u8; 32], String> {
    let mut lines = headers.get_all(name).iter().peekable();
    if lines.peek().is_none() {
        return Ok(Sha256::digest([0x00]).into());
    }

    let values = lines
        .map(|value| {
            value
                .to_str()
                .map(|value| value.trim_matches([' ', '\t']))
                .map_err(|_| format!("header {name} has an unsupported value"))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut hasher = Sha256::new();
    hasher.update([0x01]);
    hasher.update(values.join(", "));
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hash(url: &str) -> String {
        hex::encode(http_request_hash("GET", url, b"", &[], &HeaderMap::new()).unwrap())
    }

    #[test]
    fn spec_vectors() {
        assert_eq!(
            hash("https://api.example.com/article/A"),
            "0d6623f775e025501fa7f0a30b54da25aad62b6ccfe35c85da38016711e6c018"
        );
        assert_eq!(
            hash("https://api.example.com/article/B"),
            "4a99860f75eed1ea8178a5db488e044173bc570c8a6210f2c8590cdf8622d509"
        );
    }

    #[test]
    fn method_and_body_change_hash() {
        let url = "https://api.example.com/article/A";
        let get = http_request_hash("GET", url, b"", &[], &HeaderMap::new()).unwrap();
        let post = http_request_hash("POST", url, b"", &[], &HeaderMap::new()).unwrap();
        let body = http_request_hash("GET", url, b"x", &[], &HeaderMap::new()).unwrap();
        assert_ne!(get, post);
        assert_ne!(get, body);
    }

    #[test]
    fn bound_headers_distinguish_absent_and_empty() {
        let url = "https://api.example.com/article/A";
        let absent = http_request_hash("GET", url, b"", &["accept"], &HeaderMap::new()).unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("accept", HeaderValue::from_static(""));
        let empty = http_request_hash("GET", url, b"", &["accept"], &headers).unwrap();

        headers.insert("accept", HeaderValue::from_static("  text/plain "));
        let trimmed = http_request_hash("GET", url, b"", &["accept"], &headers).unwrap();
        headers.insert("accept", HeaderValue::from_static("text/plain"));
        let plain = http_request_hash("GET", url, b"", &["accept"], &headers).unwrap();

        assert_ne!(absent, empty);
        assert_eq!(trimmed, plain);
    }

    #[test]
    fn multiple_header_lines_are_joined() {
        let mut split = HeaderMap::new();
        split.append("accept", HeaderValue::from_static("a"));
        split.append("accept", HeaderValue::from_static("b"));
        let mut joined = HeaderMap::new();
        joined.insert("accept", HeaderValue::from_static("a, b"));

        assert_eq!(
            header_value_hash(&split, "accept").unwrap(),
            header_value_hash(&joined, "accept").unwrap()
        );
    }

    #[test]
    fn non_ascii_bound_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("accept", HeaderValue::from_bytes(b"caf\xc3\xa9").unwrap());
        assert!(http_request_hash("GET", "https://a.test/", b"", &["accept"], &headers).is_err());
    }

    #[test]
    fn http_params_validation() {
        assert!(is_valid_http_params(&http_params(&[])));
        assert!(is_valid_http_params(
            &json!({ "headers": ["accept", "content-type"] })
        ));

        assert!(!is_valid_http_params(&json!({})));
        assert!(!is_valid_http_params(&json!({ "headers": [], "extra": 1 })));
        assert!(!is_valid_http_params(&json!({ "headers": ["Accept"] })));
        assert!(!is_valid_http_params(&json!({ "headers": ["b", "a"] })));
        assert!(!is_valid_http_params(&json!({ "headers": ["a", "a"] })));
        assert!(!is_valid_http_params(&json!({ "headers": [""] })));
        assert!(!is_valid_http_params(
            &json!({ "headers": ["payment-signature"] })
        ));
        assert!(!is_valid_http_params(&json!({ "headers": "accept" })));
    }
}
