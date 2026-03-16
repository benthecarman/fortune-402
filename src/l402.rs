use sha2::{Digest, Sha256};

use crate::token::Token;

/// Build the WWW-Authenticate header value for an L402 challenge.
pub fn build_challenge(token_base64: &str, bolt11_invoice: &str) -> String {
    format!(
        "L402 token=\"{}\", invoice=\"{}\"",
        token_base64, bolt11_invoice
    )
}

/// Parsed L402 authorization credentials.
pub struct L402Credentials {
    pub token_base64: String,
    pub preimage: Vec<u8>,
}

/// Parse an Authorization header value like: `L402 <base64_token>:<hex_preimage>`
pub fn parse_authorization(header_value: &str) -> Result<L402Credentials, String> {
    let rest = header_value
        .strip_prefix("L402 ")
        .ok_or("authorization must start with 'L402 '")?;

    let (token_part, preimage_part) = rest
        .split_once(':')
        .ok_or("authorization must contain ':' separator")?;

    let preimage = hex::decode(preimage_part).map_err(|e| format!("invalid preimage hex: {e}"))?;

    if preimage.len() != 32 {
        return Err(format!("preimage must be 32 bytes, got {}", preimage.len()));
    }

    Ok(L402Credentials {
        token_base64: token_part.to_string(),
        preimage,
    })
}

/// Verify L402 credentials:
/// 1. Deserialize and verify the token HMAC
/// 2. Check that SHA256(preimage) == payment_hash in the token
pub fn verify_l402(root_key: &[u8; 32], token_b64: &str, preimage: &[u8]) -> Result<(), String> {
    let token = Token::deserialize(token_b64)?;
    token.verify(root_key)?;

    let hash = Sha256::digest(preimage);
    if hash.as_slice() != token.payment_hash {
        return Err("preimage does not match payment hash".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn test_parse_authorization() {
        let preimage = [0xab_u8; 32];
        let preimage_hex = hex::encode(preimage);
        let header = format!("L402 dGVzdA==:{preimage_hex}");
        let creds = parse_authorization(&header).unwrap();
        assert_eq!(creds.token_base64, "dGVzdA==");
        assert_eq!(creds.preimage, preimage.to_vec());
    }

    #[test]
    fn test_verify_l402() {
        let root_key: [u8; 32] = rand::random();
        let preimage: [u8; 32] = rand::random();
        let payment_hash: [u8; 32] = Sha256::digest(preimage).into();

        let token = Token::mint(&root_key, payment_hash);
        let encoded = token.serialize();

        verify_l402(&root_key, &encoded, &preimage).unwrap();
    }

    #[test]
    fn test_verify_l402_wrong_preimage() {
        let root_key: [u8; 32] = rand::random();
        let preimage: [u8; 32] = rand::random();
        let payment_hash: [u8; 32] = Sha256::digest(preimage).into();

        let token = Token::mint(&root_key, payment_hash);
        let encoded = token.serialize();

        let wrong_preimage: [u8; 32] = rand::random();
        assert!(verify_l402(&root_key, &encoded, &wrong_preimage).is_err());
    }

    #[test]
    fn test_build_challenge() {
        let result = build_challenge("abc123", "lnbc1...");
        assert_eq!(result, "L402 token=\"abc123\", invoice=\"lnbc1...\"");
    }
}
