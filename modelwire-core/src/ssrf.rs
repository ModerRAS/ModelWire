//! SSRF (Server-Side Request Forgery) protection utilities.
//!
//! Validates URLs to prevent requests to internal/private networks.

use std::net::IpAddr;
use std::str::FromStr;

/// Result of SSRF validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsrfValidationResult {
    /// URL is safe and allowed.
    Safe,
    /// URL is blocked due to security policy.
    Blocked { reason: &'static str },
}

/// Validate a provider base URL for SSRF vulnerabilities.
///
/// This checks:
/// - Scheme is http or https (blocks file://, ftp://, etc.)
/// - Host is not localhost or loopback
/// - Host is not a private IP address
/// - Host is not a metadata IP (169.254.169.254)
/// - Host is not unspecified (0.0.0.0)
pub fn validate_provider_url(url_str: &str) -> SsrfValidationResult {
    let url_str = url_str.trim();

    let (scheme, rest) = match url_str.split_once("://") {
        Some((s, r)) => (s, r),
        None => {
            return SsrfValidationResult::Blocked {
                reason: "Invalid URL format - missing scheme",
            };
        }
    };

    let scheme_lower = scheme.to_lowercase();
    if scheme_lower != "http" && scheme_lower != "https" {
        return SsrfValidationResult::Blocked {
            reason: "Only HTTP(S) schemes allowed",
        };
    }

    let host = match extract_host_from_url(rest) {
        Some(host) if !host.is_empty() => host,
        _ => {
            return SsrfValidationResult::Blocked {
                reason: "Invalid URL format - missing host",
            };
        }
    };

    let host_lower = host.trim_end_matches('.').to_ascii_lowercase();
    if host_lower == "localhost" {
        return SsrfValidationResult::Blocked {
            reason: "localhost not allowed",
        };
    }

    if is_blocked_metadata_hostname(&host_lower) {
        return SsrfValidationResult::Blocked {
            reason: "Metadata hostname not allowed",
        };
    }

    if let Ok(ip) = IpAddr::from_str(host.trim_end_matches('.')) {
        if ip.is_loopback() {
            return SsrfValidationResult::Blocked {
                reason: "Loopback address not allowed",
            };
        }

        if let IpAddr::V4(ipv4) = ip {
            if ipv4.is_private() {
                return SsrfValidationResult::Blocked {
                    reason: "Private IP not allowed",
                };
            }
            if ipv4.is_link_local() {
                return SsrfValidationResult::Blocked {
                    reason: "Link-local address not allowed",
                };
            }
        }

        if let IpAddr::V6(ipv6) = ip {
            let seg0 = ipv6.segments()[0];
            if seg0 == 0xfc00 || seg0 == 0xfd00 || seg0 == 0xfe80 {
                return SsrfValidationResult::Blocked {
                    reason: "IPv6 private/link-local address not allowed",
                };
            }
        }

        if ip.is_multicast() {
            return SsrfValidationResult::Blocked {
                reason: "Multicast address not allowed",
            };
        }

        if ip.is_unspecified() {
            return SsrfValidationResult::Blocked {
                reason: "Unspecified address not allowed",
            };
        }

        if let IpAddr::V4(ipv4) = ip {
            let octets = ipv4.octets();
            if octets[0] == 169 && octets[1] == 254 {
                return SsrfValidationResult::Blocked {
                    reason: "Metadata IP not allowed",
                };
            }
        }
    }

    SsrfValidationResult::Safe
}

/// Validate provider URL, allowing private IPs if the provider explicitly permits it.
pub fn validate_provider_url_for_provider(
    url_str: &str,
    allow_private_ips: bool,
) -> SsrfValidationResult {
    let url_str = url_str.trim();

    match validate_provider_url(url_str) {
        SsrfValidationResult::Blocked { reason } => {
            // Check if this is a private IP block that could be allowed
            if allow_private_ips && is_private_ip_block(reason) {
                // Parse and check if it's actually a private IP
                let host = url_str
                    .split_once("://")
                    .and_then(|(_, rest)| extract_host_from_url(rest));
                if let Some(h) = host {
                    if let Ok(ip) = IpAddr::from_str(h.trim_end_matches('.')) {
                        if is_private_or_local_ip(&ip) {
                            return SsrfValidationResult::Safe;
                        }
                    }
                }
            }
            SsrfValidationResult::Blocked { reason }
        }
        result => result,
    }
}

fn extract_host_from_url(url_str: &str) -> Option<String> {
    let host_and_port = url_str.split('/').next().unwrap_or(url_str);
    if host_and_port.is_empty() {
        return None;
    }

    if let Some(stripped) = host_and_port.strip_prefix('[') {
        let end = stripped.find(']')?;
        return Some(stripped[..end].to_string());
    }

    Some(
        host_and_port
            .split_once(':')
            .map(|(host, _)| host)
            .unwrap_or(host_and_port)
            .to_string(),
    )
}

fn is_private_ip_block(reason: &str) -> bool {
    matches!(
        reason,
        "Private IP not allowed"
            | "Loopback address not allowed"
            | "IPv6 private/link-local address not allowed"
    )
}

fn is_private_or_local_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => ipv4.is_loopback() || ipv4.is_private() || ipv4.is_link_local(),
        IpAddr::V6(ipv6) => {
            let seg0 = ipv6.segments()[0];
            seg0 == 0xfc00 || seg0 == 0xfd00 || seg0 == 0xfe80 || ipv6.is_loopback()
        }
    }
}

fn is_blocked_metadata_hostname(host_lower: &str) -> bool {
    matches!(host_lower, "metadata.google.internal" | "metadata")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_localhost() {
        assert!(matches!(
            validate_provider_url("http://localhost:8080"),
            SsrfValidationResult::Blocked { .. }
        ));
        assert!(matches!(
            validate_provider_url("https://localhost/api"),
            SsrfValidationResult::Blocked { .. }
        ));
    }

    #[test]
    fn test_block_loopback() {
        assert!(matches!(
            validate_provider_url("http://127.0.0.1:8080"),
            SsrfValidationResult::Blocked { .. }
        ));
        assert!(matches!(
            validate_provider_url("http://[::1]:8080"),
            SsrfValidationResult::Blocked { .. }
        ));
    }

    #[test]
    fn test_allow_private_ips_with_flag() {
        // Private IPs (10.x, 172.x, 192.x) can be allowed with flag
        assert!(matches!(
            validate_provider_url_for_provider("http://10.0.0.1:8080", true),
            SsrfValidationResult::Safe
        ));
        assert!(matches!(
            validate_provider_url_for_provider("https://api.openai.com/v1", true),
            SsrfValidationResult::Safe
        ));
    }

    #[test]
    fn test_block_private_ips() {
        assert!(matches!(
            validate_provider_url("http://10.0.0.1:8080"),
            SsrfValidationResult::Blocked { .. }
        ));
        assert!(matches!(
            validate_provider_url("http://172.16.0.1:8080"),
            SsrfValidationResult::Blocked { .. }
        ));
        assert!(matches!(
            validate_provider_url("http://192.168.1.1:8080"),
            SsrfValidationResult::Blocked { .. }
        ));
    }

    #[test]
    fn test_block_metadata_ip() {
        assert!(matches!(
            validate_provider_url("http://169.254.169.254/latest/meta-data"),
            SsrfValidationResult::Blocked { .. }
        ));
        assert!(matches!(
            validate_provider_url("http://metadata.google.internal/"),
            SsrfValidationResult::Blocked { .. }
        ));
    }

    #[test]
    fn test_allow_public_urls() {
        assert!(matches!(
            validate_provider_url("https://api.openai.com/v1"),
            SsrfValidationResult::Safe
        ));
        assert!(matches!(
            validate_provider_url("https://api.anthropic.com"),
            SsrfValidationResult::Safe
        ));
    }

    #[test]
    fn test_block_non_http_schemes() {
        assert!(matches!(
            validate_provider_url("file:///etc/passwd"),
            SsrfValidationResult::Blocked { .. }
        ));
        assert!(matches!(
            validate_provider_url("ftp://example.com"),
            SsrfValidationResult::Blocked { .. }
        ));
    }

    #[test]
    fn test_allow_private_with_flag() {
        assert!(matches!(
            validate_provider_url_for_provider("http://10.0.0.1:8080", true),
            SsrfValidationResult::Safe
        ));
        assert!(matches!(
            validate_provider_url_for_provider("https://api.openai.com/v1", true),
            SsrfValidationResult::Safe
        ));
    }
}
