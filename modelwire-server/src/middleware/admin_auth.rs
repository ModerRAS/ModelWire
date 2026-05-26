//! Admin API authentication and CSRF middleware.

use argon2::{
    password_hash::{PasswordHash, PasswordVerifier},
    Argon2,
};
use axum::{
    extract::{Request, State},
    http::{header::AUTHORIZATION, HeaderMap, Method},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::error::error_response_to_response;
use crate::ServerState;
use modelwire_core::error::{Error, ErrorKind};

const ADMIN_COOKIE_NAME: &str = "admin_session";
const ADMIN_CSRF_COOKIE_NAME: &str = "admin_csrf";
const ADMIN_CSRF_HEADER_NAME: &str = "x-csrf-token";

/// Middleware that enforces admin API auth and CSRF rules.
pub async fn admin_auth(
    State(state): State<Arc<ServerState>>,
    request: Request,
    next: Next,
) -> Response {
    let auth_mode = state.config.security.admin_auth.as_str();

    if auth_mode == "none" {
        return next.run(request).await;
    }

    if auth_mode != "local_password" {
        return error_response_to_response(
            Error::new(
                ErrorKind::InternalError,
                format!("Unsupported admin auth mode: {auth_mode}"),
            )
            .to_response(),
        );
    }

    if !is_local_password_auth_valid(request.headers(), state.as_ref()) {
        return error_response_to_response(
            Error::new(ErrorKind::AuthFailed, "Admin authentication required").to_response(),
        );
    }

    if is_state_changing_method(request.method()) && !is_csrf_valid(request.headers()) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(
                Error::new(ErrorKind::RequestInvalid, "Missing or invalid CSRF token")
                    .to_response(),
            ),
        )
            .into_response();
    }

    next.run(request).await
}

fn is_local_password_auth_valid(headers: &HeaderMap, state: &ServerState) -> bool {
    if let Some(expected_password) = state.config.security.admin_password.as_deref() {
        return matches_admin_password(headers, expected_password);
    }

    false
}

fn matches_admin_password(headers: &HeaderMap, expected_password: &str) -> bool {
    let provided = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(strip_bearer);
    let Some(provided_password) = provided else {
        return false;
    };

    if expected_password.starts_with("$argon2") {
        return verify_argon2_password(expected_password, provided_password);
    }

    constant_time_equal(expected_password.as_bytes(), provided_password.as_bytes())
}

fn verify_argon2_password(hash: &str, candidate: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(value) => value,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(candidate.as_bytes(), &parsed)
        .is_ok()
}

fn is_state_changing_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn is_csrf_valid(headers: &HeaderMap) -> bool {
    let session_cookie = extract_cookie(headers, ADMIN_COOKIE_NAME);
    // Bearer-authenticated non-browser clients may not use cookie sessions.
    // Enforce CSRF only when a session cookie is present.
    if session_cookie.is_none() {
        return true;
    }

    let csrf_header = headers
        .get(ADMIN_CSRF_HEADER_NAME)
        .and_then(|value| value.to_str().ok());
    let Some(csrf_header) = csrf_header else {
        return false;
    };
    let csrf_cookie = extract_cookie(headers, ADMIN_CSRF_COOKIE_NAME);
    let Some(csrf_cookie) = csrf_cookie else {
        return false;
    };
    constant_time_equal(csrf_header.as_bytes(), csrf_cookie.as_bytes())
}

fn extract_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie_headers = headers.get_all("cookie");
    for cookie_header in cookie_headers {
        let Ok(raw) = cookie_header.to_str() else {
            continue;
        };
        for part in raw.split(';') {
            let mut iter = part.trim().splitn(2, '=');
            let Some(key) = iter.next() else {
                continue;
            };
            let Some(value) = iter.next() else {
                continue;
            };
            if key.trim() == name {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

fn strip_bearer(value: &str) -> Option<&str> {
    value.strip_prefix("Bearer ")
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}
