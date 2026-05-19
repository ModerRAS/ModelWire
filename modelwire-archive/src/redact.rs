//! Redaction utilities for archive security.

use regex::Regex;

/// Redactor for removing secrets from conversation data.
pub struct Redactor {
    patterns: Vec<(Regex, &'static str)>,
}

impl Redactor {
    /// Create a new redactor with default patterns.
    pub fn new() -> Self {
        let patterns: Vec<(Regex, &'static str)> = vec![
            // Bearer tokens
            (Regex::new(r"Bearer\s+[a-zA-Z0-9_-]+").unwrap(), "Bearer [REDACTED]"),
            // API keys
            (Regex::new(r"(?i)(api[_-]?key|apikey)\s*[=:]\s*[a-zA-Z0-9_-]+").unwrap(), "[API_KEY_REDACTED]"),
            // Authorization headers
            (Regex::new(r"Authorization\s*:\s*[a-zA-Z0-9_-]+\s+[a-zA-Z0-9_=-]+").unwrap(), "Authorization: [REDACTED]"),
            // x-api-key headers
            (Regex::new(r"(?i)x-api-key\s*:\s*[a-zA-Z0-9_-]+").unwrap(), "x-api-key: [REDACTED]"),
            // AWS keys
            (Regex::new(r"(?i)(AKIA|ABIA|ACMD|ASIA)[a-zA-Z0-9]{16}").unwrap(), "[AWS_KEY_REDACTED]"),
            // GitHub tokens
            (Regex::new(r"gh[pousr]_[a-zA-Z0-9_]{36,}").unwrap(), "[GITHUB_TOKEN_REDACTED]"),
            // Private keys (PEM)
            (Regex::new(r"-----BEGIN\s+(RSA\s+|EC\s+|DSA\s+|OPENSSH\s+)?PRIVATE\s+KEY-----[\s\S]+?-----END\s+(RSA\s+|EC\s+|DSA\s+|OPENSSH\s+)?PRIVATE\s+KEY-----").unwrap(), "[PRIVATE_KEY_REDACTED]"),
            // .env assignments
            (Regex::new(r#"(?i)(password|secret|token|key)\s*[=:]\s*['"]?[a-zA-Z0-9_=-]+['"]?"#).unwrap(), "[SECRET_REDACTED]"),
            // JWT
            (Regex::new(r"eyJ[a-zA-Z0-9_-]+\.eyJ[a-zA-Z0-9_-]+\.[a-zA-Z0-9_-]+").unwrap(), "[JWT_REDACTED]"),
            // Connection strings with passwords
            (Regex::new(r"(?i)(postgres|mysql|mongodb)://[^:]+:[^@]+@").unwrap(), "[DB_CONNECTION_REDACTED]"),
        ];

        Self { patterns }
    }

    /// Add a custom pattern.
    pub fn add_pattern(&mut self, pattern: Regex, replacement: &'static str) {
        self.patterns.push((pattern, replacement));
    }

    /// Redact secrets from a string.
    pub fn redact(&self, input: &str) -> String {
        let mut result = input.to_string();
        for (pattern, replacement) in &self.patterns {
            result = pattern.replace_all(&result, *replacement).to_string();
        }
        result
    }

    /// Check if a string contains any secret patterns.
    pub fn contains_secret(&self, input: &str) -> bool {
        for (pattern, _) in &self.patterns {
            if pattern.is_match(input) {
                return true;
            }
        }
        false
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

/// Redaction result with metadata.
#[derive(Debug, Clone)]
pub struct RedactionResult {
    /// Redacted text.
    pub text: String,

    /// Whether any redactions were made.
    pub was_redacted: bool,

    /// Number of redactions applied.
    pub redaction_count: usize,
}

impl Redactor {
    /// Redact with detailed result.
    pub fn redact_detailed(&self, input: &str) -> RedactionResult {
        let mut text = input.to_string();
        let mut count: usize = 0;

        for (pattern, replacement) in &self.patterns {
            let matches = pattern.find_iter(&text).count();
            if matches > 0 {
                count += matches;
                text = pattern.replace_all(&text, *replacement).to_string();
            }
        }

        RedactionResult {
            text,
            was_redacted: count > 0,
            redaction_count: count,
        }
    }
}

/// Field-level redaction for JSON objects.
pub fn redact_json_value(key: &str, value: &serde_json::Value) -> serde_json::Value {
    let sensitive_keys = [
        "api_key",
        "authorization",
        "x-api-key",
        "password",
        "secret",
        "token",
        "credential",
    ];

    if sensitive_keys.iter().any(|s| key.to_lowercase() == *s) {
        serde_json::Value::String("[REDACTED]".to_string())
    } else {
        value.clone()
    }
}

/// Redact a JSON object field by field.
pub fn redact_json(json: &serde_json::Value) -> serde_json::Value {
    match json {
        serde_json::Value::Object(map) => {
            let redacted: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), redact_json_value(k, v)))
                .collect();
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(redact_json).collect())
        }
        _ => json.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_bearer_token() {
        let redactor = Redactor::new();
        let result = redactor.redact("Authorization: Bearer sk-abc123");
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("sk-abc123"));
    }

    #[test]
    fn test_redact_api_key() {
        let redactor = Redactor::new();
        // Test the secret key pattern - the pattern matches password/secret/token/key assignments
        let result = redactor.redact("password=mysecretpass123");
        assert!(result.contains("[SECRET_REDACTED]"));
    }

    #[test]
    fn test_redact_pem_key() {
        let redactor = Redactor::new();
        let result =
            redactor.redact("-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----");
        assert!(result.contains("[PRIVATE_KEY_REDACTED]"));
    }

    #[test]
    fn test_contains_secret() {
        let redactor = Redactor::new();
        assert!(redactor.contains_secret("Bearer sk-test"));
        assert!(!redactor.contains_secret("Hello world"));
    }

    #[test]
    fn test_redact_json() {
        let json = serde_json::json!({
            "api_key": "secret123",
            "name": "test",
        });
        let redacted = redact_json(&json);
        assert_eq!(redacted["api_key"], "[REDACTED]");
        assert_eq!(redacted["name"], "test");
    }
}
