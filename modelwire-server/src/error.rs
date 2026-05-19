//! Error handling for the server.

use axum::{
    response::{IntoResponse, Response},
    Json,
};

pub use modelwire_core::error::{Error, ErrorKind, ErrorResponse};

/// Convert an ErrorResponse to an Axum Response with appropriate HTTP status code.
pub fn error_response_to_response(error_response: ErrorResponse) -> Response {
    let status = error_response
        .error
        .error_type
        .as_ref()
        .and_then(|t| {
            let kind = match t.as_str() {
                "auth_failed" => Some(axum::http::StatusCode::UNAUTHORIZED),
                "rate_limited" => Some(axum::http::StatusCode::TOO_MANY_REQUESTS),
                "model_not_found" => Some(axum::http::StatusCode::NOT_FOUND),
                "request_too_large" => Some(axum::http::StatusCode::PAYLOAD_TOO_LARGE),
                "state_not_found" => Some(axum::http::StatusCode::NOT_FOUND),
                "state_not_continuable" => Some(axum::http::StatusCode::CONFLICT),
                "context_length_exceeded"
                | "request_invalid"
                | "protocol_not_supported"
                | "tool_mapping_failed" => Some(axum::http::StatusCode::BAD_REQUEST),
                "upstream_timeout" => Some(axum::http::StatusCode::GATEWAY_TIMEOUT),
                "upstream_unavailable" => Some(axum::http::StatusCode::BAD_GATEWAY),
                "state_replay_failed" | "internal_error" => {
                    Some(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                }
                _ => None,
            };
            kind
        })
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);

    (status, Json(error_response)).into_response()
}
