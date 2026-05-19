//! ID generation for ModelWire entities.
//!
//! ModelWire owns all downstream IDs. Upstream IDs are stored privately.

use uuid::Uuid;

/// Prefix for request IDs.
pub const REQUEST_ID_PREFIX: &str = "req_mw_";

/// Prefix for response IDs.
pub const RESPONSE_ID_PREFIX: &str = "resp_mw_";

/// Prefix for message/item IDs.
pub const MESSAGE_ID_PREFIX: &str = "msg_mw_";

/// Prefix for function call IDs.
pub const CALL_ID_PREFIX: &str = "call_mw_";

/// Prefix for function call output IDs.
pub const OUTPUT_ID_PREFIX: &str = "out_mw_";

/// Generate a new request ID.
pub fn generate_request_id() -> String {
    format!("{}{}", REQUEST_ID_PREFIX, Uuid::now_v7())
}

/// Generate a new response ID.
pub fn generate_response_id() -> String {
    format!("{}{}", RESPONSE_ID_PREFIX, Uuid::now_v7())
}

/// Generate a new message/item ID.
pub fn generate_message_id() -> String {
    format!("{}{}", MESSAGE_ID_PREFIX, Uuid::now_v7())
}

/// Generate a new function call ID.
pub fn generate_call_id() -> String {
    format!("{}{}", CALL_ID_PREFIX, Uuid::now_v7())
}

/// Generate a new function call output ID.
pub fn generate_output_id() -> String {
    format!("{}{}", OUTPUT_ID_PREFIX, Uuid::now_v7())
}

/// Hash a secret key for logging (first 8-12 hex chars of HMAC-SHA256).
pub fn hash_key_for_logging(key: &str, server_secret: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let mut mac = HmacSha256::new_from_slice(server_secret.as_bytes())
        .expect("HMAC can take key of any size");
    mac.update(key.as_bytes());

    let result = mac.finalize();
    let bytes = result.into_bytes();

    // Return first 8 hex characters
    hex::encode(&bytes[..4])
}

/// Returns true if the string starts with a ModelWire ID prefix.
pub fn is_modelwire_id(id: &str) -> bool {
    id.starts_with(REQUEST_ID_PREFIX)
        || id.starts_with(RESPONSE_ID_PREFIX)
        || id.starts_with(MESSAGE_ID_PREFIX)
        || id.starts_with(CALL_ID_PREFIX)
        || id.starts_with(OUTPUT_ID_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_prefix() {
        let id = generate_request_id();
        assert!(
            id.starts_with("req_mw_"),
            "ID should start with req_mw_: {}",
            id
        );
        assert_eq!(id.len(), 43, "UUID v7 is 36 chars, plus prefix"); // 7 + 36 = 43
    }

    #[test]
    fn test_response_id_prefix() {
        let id = generate_response_id();
        assert!(
            id.starts_with("resp_mw_"),
            "ID should start with resp_mw_: {}",
            id
        );
    }

    #[test]
    fn test_message_id_prefix() {
        let id = generate_message_id();
        assert!(
            id.starts_with("msg_mw_"),
            "ID should start with msg_mw_: {}",
            id
        );
    }

    #[test]
    fn test_call_id_prefix() {
        let id = generate_call_id();
        assert!(
            id.starts_with("call_mw_"),
            "ID should start with call_mw_: {}",
            id
        );
    }

    #[test]
    fn test_output_id_prefix() {
        let id = generate_output_id();
        assert!(
            id.starts_with("out_mw_"),
            "ID should start with out_mw_: {}",
            id
        );
    }

    #[test]
    fn test_hash_key_for_logging() {
        let hash = hash_key_for_logging("sk-test123", "secret");
        assert_eq!(hash.len(), 8, "Hash should be 8 hex chars");
        // Same input should produce same hash
        assert_eq!(hash, hash_key_for_logging("sk-test123", "secret"));
        // Different input should produce different hash
        assert_ne!(hash, hash_key_for_logging("sk-test456", "secret"));
    }

    #[test]
    fn test_is_modelwire_id() {
        assert!(is_modelwire_id("req_mw_abc123"));
        assert!(is_modelwire_id("resp_mw_xyz789"));
        assert!(is_modelwire_id("msg_mw_foo"));
        assert!(is_modelwire_id("call_mw_bar"));
        assert!(is_modelwire_id("out_mw_baz"));
        assert!(!is_modelwire_id("chatcmpl_abc"));
        assert!(!is_modelwire_id("resp_anthropic_123"));
    }
}

// Need hex crate for encoding
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
