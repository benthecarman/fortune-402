use base64::prelude::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// A simple HMAC token binding a payment hash to our server's root key.
///
/// Format (raw bytes): payment_hash (32) || hmac_signature (32)
/// Wire format: base64 of the above
#[derive(Debug, Clone)]
pub struct Token {
    pub payment_hash: [u8; 32],
    pub signature: [u8; 32],
}

impl Token {
    /// Create a new token for the given payment hash.
    pub fn mint(root_key: &[u8; 32], payment_hash: [u8; 32]) -> Self {
        let signature = hmac_sign(root_key, &payment_hash);
        Token {
            payment_hash,
            signature,
        }
    }

    /// Verify the token's HMAC against the root key.
    pub fn verify(&self, root_key: &[u8; 32]) -> Result<(), String> {
        let expected = hmac_sign(root_key, &self.payment_hash);
        if expected != self.signature {
            return Err("invalid token signature".to_string());
        }
        Ok(())
    }

    pub fn serialize(&self) -> String {
        let mut buf = [0u8; 64];
        buf[..32].copy_from_slice(&self.payment_hash);
        buf[32..].copy_from_slice(&self.signature);
        BASE64_STANDARD.encode(buf)
    }

    pub fn deserialize(encoded: &str) -> Result<Self, String> {
        let data = BASE64_STANDARD
            .decode(encoded)
            .map_err(|e| format!("base64 decode error: {e}"))?;

        if data.len() != 64 {
            return Err(format!("token must be 64 bytes, got {}", data.len()));
        }

        let mut payment_hash = [0u8; 32];
        payment_hash.copy_from_slice(&data[..32]);
        let mut signature = [0u8; 32];
        signature.copy_from_slice(&data[32..]);

        Ok(Token {
            payment_hash,
            signature,
        })
    }
}

fn hmac_sign(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take any key size");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let root_key: [u8; 32] = rand::random();
        let payment_hash: [u8; 32] = rand::random();
        let token = Token::mint(&root_key, payment_hash);

        let encoded = token.serialize();
        let decoded = Token::deserialize(&encoded).unwrap();

        assert_eq!(decoded.payment_hash, payment_hash);
        decoded.verify(&root_key).unwrap();
    }

    #[test]
    fn tampered_signature_fails() {
        let root_key: [u8; 32] = rand::random();
        let payment_hash: [u8; 32] = rand::random();
        let token = Token::mint(&root_key, payment_hash);
        let encoded = token.serialize();
        let mut decoded = Token::deserialize(&encoded).unwrap();
        decoded.signature[0] ^= 0xff;
        assert!(decoded.verify(&root_key).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let root_key: [u8; 32] = rand::random();
        let wrong_key: [u8; 32] = rand::random();
        let payment_hash: [u8; 32] = rand::random();
        let token = Token::mint(&root_key, payment_hash);
        assert!(token.verify(&wrong_key).is_err());
    }
}
