//! Health check endpoints.

use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};

/// Health response.
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// GET /healthz - Basic health check.
pub async fn healthz() -> (StatusCode, Json<HealthResponse>) {
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
    )
}

/// GET /readyz - Readiness check (checks database).
pub async fn readyz(
    state: axum::extract::State<crate::AppState>,
) -> Result<(StatusCode, Json<HealthResponse>), axum::http::StatusCode> {
    // Check database connectivity
    match state.db.ping().await {
        Ok(_) => Ok((
            StatusCode::OK,
            Json(HealthResponse {
                status: "ok".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
        )),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_healthz_returns_200() {
        let (status, body) = healthz().await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.status, "ok");
    }
}
