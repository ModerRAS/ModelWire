//! Request ID middleware - assigns unique ID to each request.

use axum::{
    extract::{Request, State},
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::ServerState;

pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Middleware that assigns a request ID to each request.
pub async fn request_id(
    State(_state): State<Arc<ServerState>>,
    mut request: Request,
    next: Next,
) -> Response {
    // Check for existing request ID
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(modelwire_core::generate_request_id);

    // Add to request extensions for later use
    request
        .extensions_mut()
        .insert(RequestIdExt(request_id.clone()));

    // Continue and get response
    let mut response = next.run(request).await;

    // Add request ID to response headers
    response.headers_mut().insert(
        HeaderName::from_static(REQUEST_ID_HEADER),
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );

    response
}

/// Request ID extension.
#[derive(Clone)]
pub struct RequestIdExt(pub String);

impl RequestIdExt {
    pub fn get_id(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_id_ext() {
        let ext = RequestIdExt("req_mw_test123".to_string());
        assert_eq!(ext.get_id(), "req_mw_test123");
    }
}
