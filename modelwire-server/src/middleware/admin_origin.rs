//! Admin API origin guard middleware.

use axum::{
    extract::{Request, State},
    http::header::ORIGIN,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::error::error_response_to_response;
use crate::ServerState;
use modelwire_core::error::{Error, ErrorKind};

/// Middleware that enforces same-origin policy for admin API requests.
///
/// Browser requests carrying `Origin` must match configured `server.public_base_url`
/// origin when available. Requests without `Origin` are allowed (CLI/server-to-server).
pub async fn admin_origin(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    let origin = request.headers().get(ORIGIN).and_then(|v| v.to_str().ok());

    // Non-browser request or same-site navigation without Origin.
    let Some(origin) = origin else {
        return next.run(request).await;
    };

    if let Some(expected_origin) = configured_origin(&state) {
        if same_origin(origin, &expected_origin) {
            return next.run(request).await;
        }
        return error_response_to_response(
            Error::new(
                ErrorKind::AuthFailed,
                format!("Untrusted admin origin: {origin}"),
            )
            .to_response(),
        );
    }

    // Conservative fallback: when no explicit public_base_url is configured,
    // allow localhost origins only.
    if is_localhost_origin(origin) {
        return next.run(request).await;
    }

    error_response_to_response(
        Error::new(
            ErrorKind::AuthFailed,
            "Admin origin rejected: configure server.public_base_url for non-local admin access",
        )
        .to_response(),
    )
}

fn configured_origin(state: &ServerState) -> Option<String> {
    let url = state.config.server.public_base_url.as_deref()?;
    normalize_origin(url)
}

fn normalize_origin(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let scheme_split = trimmed.find("://")?;
    let scheme = &trimmed[..scheme_split].to_ascii_lowercase();
    let remainder = &trimmed[(scheme_split + 3)..];
    let authority = remainder.split('/').next().unwrap_or_default().trim();
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

fn same_origin(left: &str, right: &str) -> bool {
    let Some(left_norm) = normalize_origin(left) else {
        return false;
    };
    let Some(right_norm) = normalize_origin(right) else {
        return false;
    };
    left_norm == right_norm
}

fn is_localhost_origin(origin: &str) -> bool {
    let Some(normalized) = normalize_origin(origin) else {
        return false;
    };
    normalized.contains("://localhost")
        || normalized.contains("://127.0.0.1")
        || normalized.contains("://[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_origin_matches_ignoring_path_and_case() {
        assert!(same_origin(
            "HTTPS://MODELWIRE.EXAMPLE.COM/admin",
            "https://modelwire.example.com/"
        ));
    }

    #[test]
    fn same_origin_rejects_different_hosts() {
        assert!(!same_origin(
            "https://evil.example.com",
            "https://modelwire.example.com"
        ));
    }
}
