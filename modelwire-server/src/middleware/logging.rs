//! Request logging middleware - logs requests with redacted secrets.

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::ServerState;

/// Middleware that logs requests with redacted Authorization headers.
pub async fn logging(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();

    // Get Authorization header (redacted)
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let auth_redacted = auth_header.map(|h| {
        if let Some(stripped) = h.strip_prefix("Bearer ") {
            format!(
                "Bearer {}",
                modelwire_core::hash_key_for_logging(
                    stripped,
                    state
                        .config
                        .security
                        .log_secret
                        .as_deref()
                        .unwrap_or("default-secret"),
                )
            )
        } else {
            "[REDACTED]".to_string()
        }
    });

    // Log request start
    tracing::info!(
        method = %method,
        uri = %uri,
        auth_hash = ?auth_redacted,
        "Incoming request"
    );

    // Continue
    let response = next.run(request).await;

    // Log response
    let status = response.status().as_u16();
    tracing::info!(
        method = %method,
        uri = %uri,
        status = status,
        "Response sent"
    );

    response
}

#[cfg(test)]
mod tests {
    // Integration tests would go here
}
