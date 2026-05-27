//! Admin API endpoints.

use axum::{
    extract::{Extension, Path, State},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, patch, post},
    Json, Router,
};
use modelwire_archive::redact::redact_json;
use modelwire_core::canonical::WireApi;
use modelwire_core::hash_key_for_logging;
use modelwire_core::ssrf::{validate_provider_url_for_provider, SsrfValidationResult};
use modelwire_db::repo::admin_audit::{store_admin_audit_event, AdminAuditInsert};
use modelwire_db::repo::config_apply::{replace_admin_config_with_options, ApplyConfigOptions};
use modelwire_db::repo::logs::store_log;
use modelwire_db::repo::logs::{count_logs as count_log_rows, list_logs as list_log_rows};
use modelwire_db::repo::probes::{clear_probe_results, list_probe_results as list_probe_rows};
use modelwire_db::repo::providers::{
    delete_provider as delete_provider_row, get_provider as get_provider_row,
    insert_provider as insert_provider_row, list_providers as list_provider_rows,
    update_provider as update_provider_row, ProviderInsert, ProviderRecord, ProviderUpdate,
};
use modelwire_db::repo::routes::{
    delete_route as delete_route_row, delete_target as delete_target_row,
    get_route_by_id as get_route_by_id_row, get_target_by_id as get_target_by_id_row,
    get_targets as get_targets_row, insert_route as insert_route_row,
    insert_target as insert_target_row, list_routes as list_route_rows,
    update_route as update_route_row, update_target as update_target_row, RouteInsert, RouteRecord,
    RouteUpdate, TargetInsert, TargetRecord, TargetUpdate,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::error::error_response_to_response;
use crate::middleware::request_id::RequestIdExt;
use crate::secrets::encrypt_managed_key;
use crate::ServerState;
use modelwire_core::error::{Error, ErrorKind};

/// Admin routes.
pub fn routes() -> Router<Arc<ServerState>> {
    Router::new()
        // Providers
        .route("/providers", get(list_providers).post(create_provider))
        .route(
            "/providers/:id",
            get(get_provider)
                .patch(update_provider)
                .delete(delete_provider),
        )
        // Routes
        .route("/routes", get(list_routes).post(create_route))
        .route(
            "/routes/:id",
            get(get_route).patch(update_route).delete(delete_route),
        )
        // Route targets
        .route("/routes/:id/targets", post(create_target))
        .route("/targets/:id", patch(update_target).delete(delete_target))
        // Probes
        .route("/probes", get(list_probes))
        .route("/probes/refresh", post(refresh_probes))
        // Config
        .route("/config/export", get(export_config))
        .route("/config/import", post(import_config))
        // Logs
        .route("/logs", get(list_logs))
        // Metrics
        .route("/metrics", get(get_metrics))
}

// Provider endpoints
async fn list_providers(state: State<Arc<ServerState>>) -> Json<Vec<serde_json::Value>> {
    let providers = list_provider_rows(&state.db).await.unwrap_or_default();
    Json(
        providers
            .iter()
            .map(serialize_provider_record)
            .collect::<Vec<_>>(),
    )
}

async fn create_provider(
    state: State<Arc<ServerState>>,
    Extension(request_id): Extension<RequestIdExt>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let provider = match serde_json::from_value::<modelwire_core::ProviderConfig>(body) {
        Ok(provider) => provider,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::RequestInvalid,
                    format!("Invalid provider payload: {error}"),
                )
                .to_response(),
            )
        }
    };

    if get_provider_row(&state.db, &provider.id)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return error_response_to_response(
            Error::new(
                ErrorKind::RequestInvalid,
                format!("Provider '{}' already exists", provider.id),
            )
            .to_response(),
        );
    }

    if let Err(error) = validate_provider_candidate(&provider) {
        return error_response_to_response(error.to_response());
    }

    let managed_api_key = if provider.auth_mode == "managed" {
        match provider.api_key.as_deref() {
            Some(plaintext) => {
                let Some(secret) = state
                    .config
                    .security
                    .managed_key_encryption_secret
                    .as_deref()
                else {
                    return error_response_to_response(
                        Error::new(
                            ErrorKind::InternalError,
                            "managed_key_encryption_secret is required for managed provider keys",
                        )
                        .to_response(),
                    );
                };
                match encrypt_managed_key(plaintext, secret) {
                    Ok(ciphertext) => Some(ciphertext),
                    Err(error) => return error_response_to_response(error.to_response()),
                }
            }
            None => None,
        }
    } else {
        None
    };

    let config_json = serde_json::json!({
        "allow_private_ips": provider.allow_private_ips,
        "skip_ssrf_validation": provider.skip_ssrf_validation,
        "api_key_set": provider.api_key.is_some(),
        "managed_api_key": managed_api_key,
        "config_json": provider.config_json,
    })
    .to_string();
    let insert = ProviderInsert {
        id: &provider.id,
        name: &provider.name,
        base_url: &provider.base_url,
        auth_mode: &provider.auth_mode,
        default_wire_api: &provider.default_wire_api,
        state_scope: provider.state_scope.as_deref(),
        config_json: &config_json,
    };
    if let Err(error) = insert_provider_row(&state.db, &insert).await {
        return error_response_to_response(
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to persist provider: {error}"),
            )
            .to_response(),
        );
    }

    write_admin_audit_event(
        state.as_ref(),
        request_id.get_id(),
        &admin_actor_hash(state.as_ref(), &headers),
        "provider_create",
        "provider",
        &provider.id,
        &to_redacted_diff_json(serde_json::Value::Null, serde_json::json!(provider)),
    )
    .await;

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": provider.id,
            "status": "created",
            "provider": serialize_provider_record(&ProviderRecord {
                id: provider.id,
                name: provider.name,
                base_url: provider.base_url,
                auth_mode: provider.auth_mode,
                default_wire_api: provider.default_wire_api,
                state_scope: provider.state_scope,
                config_json,
            }),
        })),
    )
        .into_response()
}

async fn get_provider(state: State<Arc<ServerState>>, Path(id): Path<String>) -> impl IntoResponse {
    match get_provider_row(&state.db, &id).await {
        Ok(Some(provider)) => Json(serialize_provider_record(&provider)).into_response(),
        Ok(None) => error_response_to_response(
            Error::new(
                ErrorKind::StateNotFound,
                format!("Provider '{id}' not found"),
            )
            .to_response(),
        ),
        Err(error) => error_response_to_response(
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to read provider '{id}': {error}"),
            )
            .to_response(),
        ),
    }
}

async fn update_provider(
    state: State<Arc<ServerState>>,
    Extension(request_id): Extension<RequestIdExt>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let existing = match get_provider_row(&state.db, &id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::StateNotFound,
                    format!("Provider '{id}' not found"),
                )
                .to_response(),
            )
        }
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to read provider '{id}': {error}"),
                )
                .to_response(),
            )
        }
    };

    let patch = match serde_json::from_value::<ProviderUpdatePayload>(body) {
        Ok(patch) => patch,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::RequestInvalid,
                    format!("Invalid provider update payload: {error}"),
                )
                .to_response(),
            )
        }
    };

    let mut candidate = provider_record_to_config(&existing);
    if let Some(new_id) = patch.id {
        if new_id != id {
            return error_response_to_response(
                Error::new(
                    ErrorKind::RequestInvalid,
                    "Provider id in payload must match path id",
                )
                .to_response(),
            );
        }
        candidate.id = new_id;
    }
    if let Some(value) = patch.name {
        candidate.name = value;
    }
    if let Some(value) = patch.base_url {
        candidate.base_url = value;
    }
    if let Some(value) = patch.auth_mode {
        candidate.auth_mode = value;
    }
    if let Some(value) = patch.default_wire_api {
        candidate.default_wire_api = value;
    }
    if patch.state_scope.is_some() {
        candidate.state_scope = patch.state_scope;
    }
    if patch.api_key.is_some() {
        candidate.api_key = patch.api_key;
    }
    if let Some(value) = patch.allow_private_ips {
        candidate.allow_private_ips = value;
    }
    if let Some(value) = patch.skip_ssrf_validation {
        candidate.skip_ssrf_validation = value;
    }
    if patch.config_json.is_some() {
        candidate.config_json = patch.config_json;
    }

    if let Err(error) = validate_provider_candidate(&candidate) {
        return error_response_to_response(error.to_response());
    }

    let managed_api_key = if candidate.auth_mode == "managed" {
        match candidate.api_key.as_deref() {
            Some(plaintext) => {
                let Some(secret) = state
                    .config
                    .security
                    .managed_key_encryption_secret
                    .as_deref()
                else {
                    return error_response_to_response(
                        Error::new(
                            ErrorKind::InternalError,
                            "managed_key_encryption_secret is required for managed provider keys",
                        )
                        .to_response(),
                    );
                };
                match encrypt_managed_key(plaintext, secret) {
                    Ok(ciphertext) => Some(ciphertext),
                    Err(error) => return error_response_to_response(error.to_response()),
                }
            }
            None => serde_json::from_str::<serde_json::Value>(&existing.config_json)
                .ok()
                .and_then(|value| value.get("managed_api_key").cloned())
                .and_then(|value| value.as_str().map(ToOwned::to_owned)),
        }
    } else {
        None
    };

    let config_json = serde_json::json!({
        "allow_private_ips": candidate.allow_private_ips,
        "skip_ssrf_validation": candidate.skip_ssrf_validation,
        "api_key_set": candidate.api_key.is_some(),
        "managed_api_key": managed_api_key,
        "config_json": candidate.config_json,
    })
    .to_string();
    let update = ProviderUpdate {
        id: &id,
        name: &candidate.name,
        base_url: &candidate.base_url,
        auth_mode: &candidate.auth_mode,
        default_wire_api: &candidate.default_wire_api,
        state_scope: candidate.state_scope.as_deref(),
        config_json: &config_json,
    };
    match update_provider_row(&state.db, &update).await {
        Ok(0) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::StateNotFound,
                    format!("Provider '{id}' not found"),
                )
                .to_response(),
            )
        }
        Ok(_) => {}
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to update provider '{id}': {error}"),
                )
                .to_response(),
            )
        }
    }

    write_admin_audit_event(
        state.as_ref(),
        request_id.get_id(),
        &admin_actor_hash(state.as_ref(), &headers),
        "provider_update",
        "provider",
        &id,
        &to_redacted_diff_json(
            serialize_provider_record(&existing),
            serialize_provider_record(&ProviderRecord {
                id: candidate.id.clone(),
                name: candidate.name.clone(),
                base_url: candidate.base_url.clone(),
                auth_mode: candidate.auth_mode.clone(),
                default_wire_api: candidate.default_wire_api.clone(),
                state_scope: candidate.state_scope.clone(),
                config_json: config_json.clone(),
            }),
        ),
    )
    .await;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": id,
            "status": "updated",
            "provider": serialize_provider_record(&ProviderRecord {
                id: candidate.id,
                name: candidate.name,
                base_url: candidate.base_url,
                auth_mode: candidate.auth_mode,
                default_wire_api: candidate.default_wire_api,
                state_scope: candidate.state_scope,
                config_json,
            }),
        })),
    )
        .into_response()
}

async fn delete_provider(
    state: State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match delete_provider_row(&state.db, &id).await {
        Ok(0) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::StateNotFound,
                    format!("Provider '{id}' not found"),
                )
                .to_response(),
            )
        }
        Ok(_) => {}
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to delete provider '{id}': {error}"),
                )
                .to_response(),
            )
        }
    }
    Json(serde_json::json!({"id": id, "status": "deleted"})).into_response()
}

// Route endpoints
async fn list_routes(state: State<Arc<ServerState>>) -> Json<Vec<serde_json::Value>> {
    let route_rows = list_route_rows(&state.db).await.unwrap_or_default();
    let mut output = Vec::with_capacity(route_rows.len());
    for route in route_rows {
        let target_count = get_targets_row(&state.db, &route.id)
            .await
            .map(|targets| targets.len())
            .unwrap_or(0);
        output.push(serde_json::json!({
            "id": route.id,
            "downstream_model": route.downstream_model,
            "description": route.description,
            "enabled": route.enabled != 0,
            "target_count": target_count,
        }));
    }
    Json(output)
}

async fn create_route(
    state: State<Arc<ServerState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let route = match serde_json::from_value::<modelwire_core::RouteConfig>(body) {
        Ok(route) => route,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::RequestInvalid,
                    format!("Invalid route payload: {error}"),
                )
                .to_response(),
            )
        }
    };

    if let Err(error) = validate_route_candidate(&state.db, &route, None).await {
        return error_response_to_response(error.to_response());
    }

    let route_id = route_effective_id(&route);
    let insert = RouteInsert {
        id: &route_id,
        downstream_model: &route.downstream_model,
        description: route.description.as_deref(),
        enabled: route.enabled,
    };
    if let Err(error) = insert_route_row(&state.db, &insert).await {
        let kind = if is_unique_violation(&error) {
            ErrorKind::RequestInvalid
        } else {
            ErrorKind::InternalError
        };
        return error_response_to_response(
            Error::new(
                kind,
                format!("Failed to persist route '{}': {error}", route_id),
            )
            .to_response(),
        );
    }

    for target in &route.targets {
        if let Err(error) = insert_target_for_route(&state.db, &route_id, target).await {
            let _ = delete_route_row(&state.db, &route_id).await;
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!(
                        "Failed to persist target '{}' for route '{}': {error}",
                        target_effective_id(&route_id, target),
                        route_id
                    ),
                )
                .to_response(),
            );
        }
    }

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": route_id,
            "status": "created",
            "route": route,
        })),
    )
        .into_response()
}

async fn get_route(state: State<Arc<ServerState>>, Path(id): Path<String>) -> impl IntoResponse {
    let route = match get_route_by_id_row(&state.db, &id).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return error_response_to_response(
                Error::new(ErrorKind::StateNotFound, format!("Route '{id}' not found"))
                    .to_response(),
            )
        }
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to read route '{id}': {error}"),
                )
                .to_response(),
            )
        }
    };

    let targets = match get_targets_row(&state.db, &route.id).await {
        Ok(targets) => targets,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to read route targets for '{id}': {error}"),
                )
                .to_response(),
            )
        }
    };

    Json(serialize_route_record(&route, &targets)).into_response()
}

async fn update_route(
    state: State<Arc<ServerState>>,
    Extension(request_id): Extension<RequestIdExt>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let existing = match get_route_by_id_row(&state.db, &id).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return error_response_to_response(
                Error::new(ErrorKind::StateNotFound, format!("Route '{id}' not found"))
                    .to_response(),
            )
        }
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to read route '{id}': {error}"),
                )
                .to_response(),
            )
        }
    };

    let existing_targets = match get_targets_row(&state.db, &existing.id).await {
        Ok(targets) => targets,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to read route targets for '{id}': {error}"),
                )
                .to_response(),
            )
        }
    };

    let patch = match serde_json::from_value::<RouteUpdatePayload>(body) {
        Ok(patch) => patch,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::RequestInvalid,
                    format!("Invalid route update payload: {error}"),
                )
                .to_response(),
            )
        }
    };

    let mut candidate = route_record_to_config(&existing, &existing_targets);
    let existing_route_json = serialize_route_record(&existing, &existing_targets);
    let replace_targets = patch.targets.is_some();
    if let Some(new_id) = patch.id {
        if new_id != id {
            return error_response_to_response(
                Error::new(
                    ErrorKind::RequestInvalid,
                    "Route id in payload must match path id",
                )
                .to_response(),
            );
        }
        candidate.id = Some(new_id);
    }
    if let Some(value) = patch.downstream_model {
        candidate.downstream_model = value;
    }
    if patch.description.is_some() {
        candidate.description = patch.description;
    }
    if let Some(value) = patch.enabled {
        candidate.enabled = value;
    }
    if let Some(value) = patch.targets {
        candidate.targets = value;
    }

    if let Err(error) = validate_route_candidate(&state.db, &candidate, Some(&id)).await {
        return error_response_to_response(error.to_response());
    }

    let update = RouteUpdate {
        id: &id,
        downstream_model: &candidate.downstream_model,
        description: candidate.description.as_deref(),
        enabled: candidate.enabled,
    };
    match update_route_row(&state.db, &update).await {
        Ok(0) => {
            return error_response_to_response(
                Error::new(ErrorKind::StateNotFound, format!("Route '{id}' not found"))
                    .to_response(),
            )
        }
        Ok(_) => {}
        Err(error) => {
            let kind = if is_unique_violation(&error) {
                ErrorKind::RequestInvalid
            } else {
                ErrorKind::InternalError
            };
            return error_response_to_response(
                Error::new(kind, format!("Failed to update route '{id}': {error}")).to_response(),
            );
        }
    }

    if replace_targets {
        for target in &existing_targets {
            if let Err(error) = delete_target_row(&state.db, &target.id).await {
                return error_response_to_response(
                    Error::new(
                        ErrorKind::InternalError,
                        format!(
                            "Failed to replace targets for route '{}': {error}",
                            candidate.downstream_model
                        ),
                    )
                    .to_response(),
                );
            }
        }
        for target in &candidate.targets {
            if let Err(error) = insert_target_for_route(&state.db, &id, target).await {
                return error_response_to_response(
                    Error::new(
                        ErrorKind::InternalError,
                        format!(
                            "Failed to persist target '{}' for route '{}': {error}",
                            target_effective_id(&id, target),
                            id
                        ),
                    )
                    .to_response(),
                );
            }
        }
    }

    let updated_targets = if replace_targets {
        match get_targets_row(&state.db, &id).await {
            Ok(targets) => targets,
            Err(error) => {
                return error_response_to_response(
                    Error::new(
                        ErrorKind::InternalError,
                        format!("Failed to read updated route targets for audit: {error}"),
                    )
                    .to_response(),
                );
            }
        }
    } else {
        existing_targets.clone()
    };
    let updated_route_json = serialize_route_record(
        &RouteRecord {
            id: id.clone(),
            downstream_model: candidate.downstream_model.clone(),
            description: candidate.description.clone(),
            enabled: if candidate.enabled { 1 } else { 0 },
        },
        &updated_targets,
    );
    write_admin_audit_event(
        state.as_ref(),
        request_id.get_id(),
        &admin_actor_hash(state.as_ref(), &headers),
        "route_update",
        "route",
        &id,
        &to_redacted_diff_json(existing_route_json, updated_route_json),
    )
    .await;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": id,
            "status": "updated",
            "route": candidate,
        })),
    )
        .into_response()
}

async fn delete_route(state: State<Arc<ServerState>>, Path(id): Path<String>) -> impl IntoResponse {
    let targets = match get_targets_row(&state.db, &id).await {
        Ok(targets) => targets,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to inspect route targets for '{id}': {error}"),
                )
                .to_response(),
            )
        }
    };
    for target in targets {
        if let Err(error) = delete_target_row(&state.db, &target.id).await {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!(
                        "Failed to delete target '{}' for route '{}': {error}",
                        target.id, id
                    ),
                )
                .to_response(),
            );
        }
    }
    match delete_route_row(&state.db, &id).await {
        Ok(0) => {
            return error_response_to_response(
                Error::new(ErrorKind::StateNotFound, format!("Route '{id}' not found"))
                    .to_response(),
            )
        }
        Ok(_) => {}
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to delete route '{id}': {error}"),
                )
                .to_response(),
            )
        }
    }
    Json(serde_json::json!({"id": id, "status": "deleted"})).into_response()
}

// Target endpoints
async fn create_target(
    state: State<Arc<ServerState>>,
    Path(route_id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let route = match get_route_by_id_row(&state.db, &route_id).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::StateNotFound,
                    format!("Route '{route_id}' not found"),
                )
                .to_response(),
            )
        }
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to read route '{route_id}': {error}"),
                )
                .to_response(),
            )
        }
    };

    let target = match parse_target_payload(body) {
        Ok(target) => target,
        Err(error) => return error_response_to_response(error.to_response()),
    };

    if let Err(error) = validate_target_candidate(&state.db, &route, &target).await {
        return error_response_to_response(error.to_response());
    }

    let target_id = target_effective_id(&route.id, &target);
    if let Err(error) = insert_target_for_route(&state.db, &route.id, &target).await {
        let kind = if is_unique_violation(&error) {
            ErrorKind::RequestInvalid
        } else {
            ErrorKind::InternalError
        };
        return error_response_to_response(
            Error::new(
                kind,
                format!("Failed to persist target '{target_id}': {error}"),
            )
            .to_response(),
        );
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": target_id,
            "status": "created",
            "target": serialize_target_with_id(&route.id, &target),
        })),
    )
        .into_response()
}

async fn update_target(
    state: State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let existing = match get_target_by_id_row(&state.db, &id).await {
        Ok(Some(target)) => target,
        Ok(None) => {
            return error_response_to_response(
                Error::new(ErrorKind::StateNotFound, format!("Target '{id}' not found"))
                    .to_response(),
            )
        }
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to read target '{id}': {error}"),
                )
                .to_response(),
            )
        }
    };
    let route = match get_route_by_id_row(&state.db, &existing.route_id).await {
        Ok(Some(route)) => route,
        Ok(None) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::StateNotFound,
                    format!(
                        "Route '{}' not found for target '{}'",
                        existing.route_id, existing.id
                    ),
                )
                .to_response(),
            )
        }
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!(
                        "Failed to read route '{}' for target '{}': {error}",
                        existing.route_id, existing.id
                    ),
                )
                .to_response(),
            )
        }
    };

    let patch = match serde_json::from_value::<TargetUpdatePayload>(body) {
        Ok(patch) => patch,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::RequestInvalid,
                    format!("Invalid target update payload: {error}"),
                )
                .to_response(),
            )
        }
    };

    let mut candidate = target_record_to_config(&existing);
    if let Some(value) = patch.provider {
        candidate.provider = value;
    }
    if let Some(value) = patch.provider_id {
        candidate.provider = value;
    }
    if let Some(value) = patch.upstream_model {
        candidate.upstream_model = value;
    }
    if let Some(value) = patch.wire_api {
        candidate.wire_api = value;
    }
    if let Some(value) = patch.priority {
        candidate.priority = value;
    }
    if let Some(value) = patch.enabled {
        candidate.enabled = value;
    }
    if patch.context_window_tokens.is_some() {
        candidate.context_window_tokens = patch.context_window_tokens;
    }
    if patch.max_output_tokens.is_some() {
        candidate.max_output_tokens = patch.max_output_tokens;
    }
    if patch.auto_compact_recommended_tokens.is_some() {
        candidate.auto_compact_recommended_tokens = patch.auto_compact_recommended_tokens;
    }
    if patch.context_safety_margin_tokens.is_some() {
        candidate.context_safety_margin_tokens = patch.context_safety_margin_tokens;
    }
    if patch.token_estimator.is_some() {
        candidate.token_estimator = patch.token_estimator;
    }
    if let Some(value) = patch.context_overflow_policy {
        candidate.context_overflow_policy = value;
    }
    if patch.config_json.is_some() {
        candidate.config_json = patch.config_json;
    }

    if let Err(error) = validate_target_candidate(&state.db, &route, &candidate).await {
        return error_response_to_response(error.to_response());
    }

    let config_json = target_config_json(&candidate);
    let update = TargetUpdate {
        id: &id,
        provider_id: &candidate.provider,
        upstream_model: &candidate.upstream_model,
        wire_api: &candidate.wire_api,
        priority: candidate.priority,
        enabled: candidate.enabled,
        config_json: &config_json,
    };
    match update_target_row(&state.db, &update).await {
        Ok(0) => {
            return error_response_to_response(
                Error::new(ErrorKind::StateNotFound, format!("Target '{id}' not found"))
                    .to_response(),
            )
        }
        Ok(_) => {}
        Err(error) => {
            let kind = if is_unique_violation(&error) {
                ErrorKind::RequestInvalid
            } else {
                ErrorKind::InternalError
            };
            return error_response_to_response(
                Error::new(kind, format!("Failed to update target '{id}': {error}")).to_response(),
            );
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "id": id,
            "status": "updated",
            "target": serialize_target_with_id(&route.id, &candidate),
        })),
    )
        .into_response()
}

async fn delete_target(
    state: State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match delete_target_row(&state.db, &id).await {
        Ok(0) => {
            return error_response_to_response(
                Error::new(ErrorKind::StateNotFound, format!("Target '{id}' not found"))
                    .to_response(),
            )
        }
        Ok(_) => {}
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to delete target '{id}': {error}"),
                )
                .to_response(),
            )
        }
    }
    Json(serde_json::json!({"id": id, "status": "deleted"})).into_response()
}

// Probe endpoints
async fn list_probes(state: State<Arc<ServerState>>) -> Json<Vec<serde_json::Value>> {
    let mut probes = Vec::new();

    // Persisted probe view for admin status screen.
    let persisted = list_probe_rows(&state.db, 500).await.unwrap_or_default();
    for row in persisted {
        let key = format!(
            "{}:{}:{}",
            row.provider_id, row.credential_hash, row.upstream_model
        );
        probes.push(serde_json::json!({
            "key": key,
            "provider_id": row.provider_id,
            "credential_hash": row.credential_hash,
            "upstream_model": row.upstream_model,
            "wire_api": row.wire_api,
            "status": row.status,
            "supports_streaming": row.supports_streaming.unwrap_or(0) != 0,
            "supports_tools": row.supports_tools.unwrap_or(0) != 0,
            "supports_parallel_tool_calls": row.supports_parallel_tool_calls.unwrap_or(0) != 0,
            "supports_previous_response_id": row.supports_previous_response_id.unwrap_or(0) != 0,
            "supports_reasoning_encrypted_content": row.supports_reasoning_encrypted_content.unwrap_or(0) != 0,
            "supports_reasoning_summary": row.supports_reasoning_summary.unwrap_or(0) != 0,
            "last_success_at": row.last_success_at,
            "last_failure_at": row.last_failure_at,
            "failure_kind": row.failure_kind,
            "failure_message_redacted": row.failure_message_redacted,
            "expires_at": row.expires_at,
            "source": "persisted",
        }));
    }

    // Add in-memory cache-only probes not yet persisted.
    for entry in &state.probe_cache {
        let key = entry.key().to_string();
        if probes
            .iter()
            .any(|p| p["key"].as_str() == Some(key.as_str()))
        {
            continue;
        }
        probes.push(serde_json::json!({
            "key": key,
            "provider_id": entry.value().provider_id,
            "credential_hash": entry.value().credential_hash,
            "upstream_model": entry.value().upstream_model,
            "wire_api": entry.value().wire_api.as_str(),
            "status": "success",
            "supports_streaming": entry.value().supports_streaming,
            "supports_tools": entry.value().supports_tools,
            "supports_parallel_tool_calls": entry.value().supports_parallel_tool_calls,
            "supports_previous_response_id": entry.value().supports_previous_response_id,
            "supports_reasoning_encrypted_content": entry.value().supports_reasoning_encrypted_content,
            "supports_reasoning_summary": entry.value().supports_reasoning_summary,
            "last_success_at": entry.value().last_success_at,
            "last_failure_at": entry.value().last_failure_at,
            "failure_kind": entry.value().failure_kind,
            "failure_message_redacted": entry.value().failure_message_redacted,
            "expires_at": entry.value().expires_at,
            "source": "cache",
        }));
    }

    Json(probes)
}

async fn refresh_probes(
    state: State<Arc<ServerState>>,
    Json(_body): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Clear probe cache
    state.probe_cache.clear();
    state.probe_locks.clear();
    let cleared_rows = match clear_probe_results(&state.db).await {
        Ok(rows) => rows,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to clear persisted probe results: {error}"),
                )
                .to_response(),
            )
        }
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "probes_refreshed",
            "persisted_cleared": cleared_rows
        })),
    )
        .into_response()
}

// Config endpoints
async fn export_config(state: State<Arc<ServerState>>) -> Json<serde_json::Value> {
    Json(state.config.to_redacted_json())
}

async fn import_config(
    state: State<Arc<ServerState>>,
    Extension(request_id): Extension<RequestIdExt>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let config = match parse_import_config(&body) {
        Ok(config) => config,
        Err(error) => return error_response_to_response(error.to_response()),
    };

    if let Err(error) = validate_import_provider_urls(&config) {
        return error_response_to_response(error.to_response());
    }

    let applied = match replace_admin_config_with_options(
        &state.db,
        &config,
        ApplyConfigOptions {
            include_managed_api_keys: false,
        },
    )
    .await
    {
        Ok(applied) => applied,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to apply imported config: {error}"),
                )
                .to_response(),
            )
        }
    };

    write_admin_audit_event(
        state.as_ref(),
        request_id.get_id(),
        &admin_actor_hash(state.as_ref(), &headers),
        "config_import",
        "config",
        "runtime",
        &to_redacted_diff_json(serde_json::Value::Null, body.clone()),
    )
    .await;

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "imported",
            "providers_count": config.providers.len(),
            "routes_count": config.routes.len(),
            "targets_count": config.routes.iter().map(|route| route.targets.len()).sum::<usize>(),
            "applied": {
                "providers": applied.providers,
                "routes": applied.routes,
                "targets": applied.targets
            }
        })),
    )
        .into_response()
}

fn parse_import_config(body: &serde_json::Value) -> Result<modelwire_core::Config, Error> {
    let config =
        serde_json::from_value::<modelwire_core::Config>(body.clone()).map_err(|error| {
            Error::new(
                ErrorKind::RequestInvalid,
                format!("Invalid config import payload: {error}"),
            )
        })?;
    validate_import_config(&config)?;
    Ok(config)
}

fn validate_import_config(config: &modelwire_core::Config) -> Result<(), Error> {
    let mut provider_ids = std::collections::HashSet::new();
    for provider in &config.providers {
        if !provider_ids.insert(provider.id.as_str()) {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!(
                    "Invalid config import payload: duplicate provider id '{}'",
                    provider.id
                ),
            ));
        }
    }

    let mut downstream_models = std::collections::HashSet::new();
    for route in &config.routes {
        if !downstream_models.insert(route.downstream_model.as_str()) {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!(
                    "Invalid config import payload: duplicate downstream model '{}'",
                    route.downstream_model
                ),
            ));
        }
        for target in &route.targets {
            if !provider_ids.contains(target.provider.as_str()) {
                return Err(Error::new(
                    ErrorKind::RequestInvalid,
                    format!(
                        "Invalid config import payload: route '{}' references unknown provider '{}'",
                        route.downstream_model, target.provider
                    ),
                ));
            }
        }
    }

    Ok(())
}

fn validate_import_provider_urls(config: &modelwire_core::Config) -> Result<(), Error> {
    for provider in &config.providers {
        if provider.skip_ssrf_validation {
            continue;
        }
        match validate_provider_url_for_provider(&provider.base_url, provider.allow_private_ips) {
            SsrfValidationResult::Safe => {}
            SsrfValidationResult::Blocked { reason } => {
                return Err(Error::new(
                    ErrorKind::RequestInvalid,
                    format!(
                        "Provider '{}' base_url rejected by SSRF policy: {reason}",
                        provider.id
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn route_effective_id(route: &modelwire_core::RouteConfig) -> String {
    route
        .id
        .clone()
        .unwrap_or_else(|| route.downstream_model.clone())
}

fn target_effective_id(route_id: &str, target: &modelwire_core::TargetConfig) -> String {
    format!("{route_id}:{}:{}", target.provider, target.priority)
}

fn serialize_target_with_id(
    route_id: &str,
    target: &modelwire_core::TargetConfig,
) -> serde_json::Value {
    serde_json::json!({
        "id": target_effective_id(route_id, target),
        "route_id": route_id,
        "provider": target.provider,
        "provider_id": target.provider,
        "upstream_model": target.upstream_model,
        "wire_api": target.wire_api,
        "priority": target.priority,
        "enabled": target.enabled,
        "context_window_tokens": target.context_window_tokens,
        "max_output_tokens": target.max_output_tokens,
        "auto_compact_recommended_tokens": target.auto_compact_recommended_tokens,
        "context_safety_margin_tokens": target.context_safety_margin_tokens,
        "token_estimator": target.token_estimator,
        "context_overflow_policy": target.context_overflow_policy,
        "config_json": target.config_json,
    })
}

fn serialize_target_record(target: &TargetRecord) -> serde_json::Value {
    serialize_target_with_id(&target.route_id, &target_record_to_config(target))
}

fn serialize_route_record(route: &RouteRecord, targets: &[TargetRecord]) -> serde_json::Value {
    serde_json::json!({
        "id": route.id,
        "downstream_model": route.downstream_model,
        "description": route.description,
        "enabled": route.enabled != 0,
        "targets": targets.iter().map(serialize_target_record).collect::<Vec<_>>(),
    })
}

fn serialize_provider_record(provider: &ProviderRecord) -> serde_json::Value {
    let parsed = serde_json::from_str::<serde_json::Value>(&provider.config_json).ok();
    let api_key_set = parsed
        .as_ref()
        .and_then(|value| value.get("api_key_set"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    serde_json::json!({
        "id": provider.id,
        "name": provider.name,
        "base_url": provider.base_url,
        "auth_mode": provider.auth_mode,
        "default_wire_api": provider.default_wire_api,
        "state_scope": provider.state_scope,
        "api_key_set": api_key_set,
    })
}

fn provider_record_to_config(provider: &ProviderRecord) -> modelwire_core::ProviderConfig {
    let parsed = serde_json::from_str::<serde_json::Value>(&provider.config_json).ok();
    let allow_private_ips = parsed
        .as_ref()
        .and_then(|value| value.get("allow_private_ips"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let skip_ssrf_validation = parsed
        .as_ref()
        .and_then(|value| value.get("skip_ssrf_validation"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let config_json = parsed
        .as_ref()
        .and_then(|value| value.get("config_json"))
        .cloned();
    // Never surface stored managed key ciphertext via ProviderConfig projection.
    let api_key = None;

    modelwire_core::ProviderConfig {
        id: provider.id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        auth_mode: provider.auth_mode.clone(),
        default_wire_api: provider.default_wire_api.clone(),
        state_scope: provider.state_scope.clone(),
        api_key,
        allow_private_ips,
        skip_ssrf_validation,
        config_json,
    }
}

fn route_record_to_config(
    route: &RouteRecord,
    targets: &[TargetRecord],
) -> modelwire_core::RouteConfig {
    modelwire_core::RouteConfig {
        id: Some(route.id.clone()),
        downstream_model: route.downstream_model.clone(),
        description: route.description.clone(),
        enabled: route.enabled != 0,
        targets: targets
            .iter()
            .map(target_record_to_config)
            .collect::<Vec<_>>(),
    }
}

fn target_record_to_config(target: &TargetRecord) -> modelwire_core::TargetConfig {
    let parsed = serde_json::from_str::<serde_json::Value>(&target.config_json).ok();
    let context_window_tokens = parsed
        .as_ref()
        .and_then(|value| value.get("context_window_tokens"))
        .and_then(serde_json::Value::as_u64);
    let max_output_tokens = parsed
        .as_ref()
        .and_then(|value| value.get("max_output_tokens"))
        .and_then(serde_json::Value::as_u64);
    let auto_compact_recommended_tokens = parsed
        .as_ref()
        .and_then(|value| value.get("auto_compact_recommended_tokens"))
        .and_then(serde_json::Value::as_u64);
    let context_safety_margin_tokens = parsed
        .as_ref()
        .and_then(|value| value.get("context_safety_margin_tokens"))
        .and_then(serde_json::Value::as_u64);
    let token_estimator = parsed
        .as_ref()
        .and_then(|value| value.get("token_estimator"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let context_overflow_policy = parsed
        .as_ref()
        .and_then(|value| value.get("context_overflow_policy"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "reject".to_string());
    let config_json = parsed
        .as_ref()
        .and_then(|value| value.get("config_json"))
        .cloned();

    modelwire_core::TargetConfig {
        provider: target.provider_id.clone(),
        upstream_model: target.upstream_model.clone(),
        wire_api: target.wire_api.clone(),
        priority: target.priority,
        enabled: target.enabled != 0,
        context_window_tokens,
        max_output_tokens,
        auto_compact_recommended_tokens,
        context_safety_margin_tokens,
        token_estimator,
        context_overflow_policy,
        config_json,
    }
}

fn target_config_json(target: &modelwire_core::TargetConfig) -> String {
    serde_json::json!({
        "context_window_tokens": target.context_window_tokens,
        "max_output_tokens": target.max_output_tokens,
        "auto_compact_recommended_tokens": target.auto_compact_recommended_tokens,
        "context_safety_margin_tokens": target.context_safety_margin_tokens,
        "token_estimator": target.token_estimator,
        "context_overflow_policy": target.context_overflow_policy,
        "config_json": target.config_json,
    })
    .to_string()
}

async fn insert_target_for_route(
    db: &modelwire_db::Database,
    route_id: &str,
    target: &modelwire_core::TargetConfig,
) -> Result<(), sqlx::Error> {
    let target_id = target_effective_id(route_id, target);
    let config_json = target_config_json(target);
    let insert = TargetInsert {
        id: &target_id,
        route_id,
        provider_id: &target.provider,
        upstream_model: &target.upstream_model,
        wire_api: &target.wire_api,
        priority: target.priority,
        enabled: target.enabled,
        config_json: &config_json,
    };
    insert_target_row(db, &insert).await
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(db_error) => {
            if db_error.is_unique_violation() {
                return true;
            }
            let message = db_error.message().to_ascii_lowercase();
            message.contains("unique constraint")
                || message.contains("duplicate key")
                || message.contains("already exists")
        }
        _ => false,
    }
}

fn admin_actor_hash(state: &ServerState, headers: &HeaderMap) -> String {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("admin");
    let secret = state
        .config
        .security
        .log_secret
        .as_deref()
        .unwrap_or("modelwire-admin-audit-default-secret");
    format!("admin:{}", hash_key_for_logging(token, secret))
}

fn to_redacted_diff_json(before: serde_json::Value, after: serde_json::Value) -> String {
    serde_json::json!({
        "before": redact_json(&before),
        "after": redact_json(&after)
    })
    .to_string()
}

async fn write_admin_audit_event(
    state: &ServerState,
    request_id: &str,
    actor_hash: &str,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    diff_json: &str,
) {
    if let Err(error) = store_admin_audit_event(
        &state.db,
        &AdminAuditInsert {
            id: &format!("audit_{}", uuid::Uuid::new_v4()),
            request_id,
            actor_key_hash: actor_hash,
            action,
            resource_type,
            resource_id,
            diff_json,
        },
    )
    .await
    {
        let _ = store_log(
            &state.db,
            request_id,
            Some(actor_hash),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(500),
            Some("admin_audit_write_failed"),
            None,
            None,
            None,
        )
        .await;
        tracing::warn!(
            request_id = %request_id,
            action = %action,
            resource_type = %resource_type,
            resource_id = %resource_id,
            error = %error,
            "Failed to persist admin audit event"
        );
    }
}

fn validate_provider_candidate(provider: &modelwire_core::ProviderConfig) -> Result<(), Error> {
    if provider.id.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Provider id must not be empty",
        ));
    }
    if provider.name.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Provider name must not be empty",
        ));
    }
    if provider.base_url.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Provider base_url must not be empty",
        ));
    }
    if provider.skip_ssrf_validation {
        return Ok(());
    }
    match validate_provider_url_for_provider(&provider.base_url, provider.allow_private_ips) {
        SsrfValidationResult::Safe => Ok(()),
        SsrfValidationResult::Blocked { reason } => Err(Error::new(
            ErrorKind::RequestInvalid,
            format!(
                "Provider '{}' base_url rejected by SSRF policy: {reason}",
                provider.id
            ),
        )),
    }
}

async fn validate_route_candidate(
    db: &modelwire_db::Database,
    route: &modelwire_core::RouteConfig,
    existing_route_id: Option<&str>,
) -> Result<(), Error> {
    if route.downstream_model.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Route downstream_model must not be empty",
        ));
    }
    if route.targets.is_empty() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            format!(
                "Route '{}' must include at least one target",
                route.downstream_model
            ),
        ));
    }

    let candidate_id = route_effective_id(route);
    let existing_routes = list_route_rows(db).await.map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to validate route candidate: {error}"),
        )
    })?;

    for existing in &existing_routes {
        if Some(existing.id.as_str()) == existing_route_id {
            continue;
        }
        if existing.id == candidate_id {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!("Route '{}' already exists", candidate_id),
            ));
        }
        if existing.downstream_model == route.downstream_model {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!(
                    "Route downstream model '{}' already exists",
                    route.downstream_model
                ),
            ));
        }
    }

    let mut seen_target_ids = std::collections::HashSet::new();
    for target in &route.targets {
        validate_target_candidate(
            db,
            &RouteRecord {
                id: candidate_id.clone(),
                downstream_model: route.downstream_model.clone(),
                description: route.description.clone(),
                enabled: if route.enabled { 1 } else { 0 },
            },
            target,
        )
        .await?;
        let target_id = target_effective_id(&candidate_id, target);
        if !seen_target_ids.insert(target_id.clone()) {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!(
                    "Route '{}' contains duplicate target id '{}'",
                    route.downstream_model, target_id
                ),
            ));
        }
    }
    Ok(())
}

fn parse_target_payload(body: serde_json::Value) -> Result<modelwire_core::TargetConfig, Error> {
    if let Ok(target) = serde_json::from_value::<modelwire_core::TargetConfig>(body.clone()) {
        return Ok(target);
    }
    if let Ok(alias_target) = serde_json::from_value::<TargetCreatePayloadAlias>(body) {
        return Ok(modelwire_core::TargetConfig {
            provider: alias_target.provider_id,
            upstream_model: alias_target.upstream_model,
            wire_api: alias_target.wire_api.unwrap_or_else(|| "auto".to_string()),
            priority: alias_target.priority.unwrap_or(10),
            enabled: alias_target.enabled.unwrap_or(true),
            context_window_tokens: alias_target.context_window_tokens,
            max_output_tokens: alias_target.max_output_tokens,
            auto_compact_recommended_tokens: alias_target.auto_compact_recommended_tokens,
            context_safety_margin_tokens: alias_target.context_safety_margin_tokens,
            token_estimator: alias_target.token_estimator,
            context_overflow_policy: alias_target
                .context_overflow_policy
                .unwrap_or_else(|| "reject".to_string()),
            config_json: alias_target.config_json,
        });
    }
    Err(Error::new(
        ErrorKind::RequestInvalid,
        "Invalid target payload",
    ))
}

async fn validate_target_candidate(
    db: &modelwire_db::Database,
    route: &RouteRecord,
    target: &modelwire_core::TargetConfig,
) -> Result<(), Error> {
    if target.provider.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Target provider must not be empty",
        ));
    }
    if target.upstream_model.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            "Target upstream_model must not be empty",
        ));
    }
    if get_provider_row(db, &target.provider)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to validate provider reference: {error}"),
            )
        })?
        .is_none()
    {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            format!(
                "Route '{}' references unknown provider '{}'",
                route.downstream_model, target.provider
            ),
        ));
    }
    if WireApi::parse(&target.wire_api).is_none() {
        return Err(Error::new(
            ErrorKind::RequestInvalid,
            format!(
                "Target '{}' has invalid wire_api '{}'",
                target_effective_id(&route.id, target),
                target.wire_api
            ),
        ));
    }
    match target.context_overflow_policy.as_str() {
        "reject" | "fallback" | "summarize_explicit" => {}
        other => {
            return Err(Error::new(
                ErrorKind::RequestInvalid,
                format!(
                    "Target '{}' has invalid context_overflow_policy '{}'",
                    target_effective_id(&route.id, target),
                    other
                ),
            ))
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, Default)]
struct ProviderUpdatePayload {
    id: Option<String>,
    name: Option<String>,
    base_url: Option<String>,
    auth_mode: Option<String>,
    default_wire_api: Option<String>,
    state_scope: Option<String>,
    api_key: Option<String>,
    allow_private_ips: Option<bool>,
    skip_ssrf_validation: Option<bool>,
    config_json: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct RouteUpdatePayload {
    id: Option<String>,
    downstream_model: Option<String>,
    description: Option<String>,
    enabled: Option<bool>,
    targets: Option<Vec<modelwire_core::TargetConfig>>,
}

#[derive(Debug, Deserialize)]
struct TargetCreatePayloadAlias {
    provider_id: String,
    upstream_model: String,
    #[serde(default)]
    wire_api: Option<String>,
    #[serde(default)]
    priority: Option<i32>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    context_window_tokens: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    auto_compact_recommended_tokens: Option<u64>,
    #[serde(default)]
    context_safety_margin_tokens: Option<u64>,
    #[serde(default)]
    token_estimator: Option<String>,
    #[serde(default)]
    context_overflow_policy: Option<String>,
    #[serde(default)]
    config_json: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct TargetUpdatePayload {
    provider: Option<String>,
    provider_id: Option<String>,
    upstream_model: Option<String>,
    wire_api: Option<String>,
    priority: Option<i32>,
    enabled: Option<bool>,
    context_window_tokens: Option<u64>,
    max_output_tokens: Option<u64>,
    auto_compact_recommended_tokens: Option<u64>,
    context_safety_margin_tokens: Option<u64>,
    token_estimator: Option<String>,
    context_overflow_policy: Option<String>,
    config_json: Option<serde_json::Value>,
}

// Logs endpoint
async fn list_logs(
    State(state): State<Arc<ServerState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);

    let rows = list_log_rows(&state.db, limit as i64)
        .await
        .unwrap_or_default();
    let total = count_log_rows(&state.db).await.unwrap_or(rows.len() as i64);

    let logs = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "id": row.id,
                "request_id": row.request_id,
                "downstream_key_hash": row.downstream_key_hash,
                "downstream_model": row.downstream_model,
                "route_id": row.route_id,
                "target_id": row.target_id,
                "provider_id": row.provider_id,
                "upstream_model": row.upstream_model,
                "wire_api": row.wire_api,
                "status_code": row.status_code,
                "error_kind": row.error_kind,
                "latency_ms": row.latency_ms,
                "input_tokens": row.input_tokens,
                "output_tokens": row.output_tokens,
                "reasoning_tokens": row.reasoning_tokens,
                "created_at": row.created_at,
            })
        })
        .collect::<Vec<_>>();

    Json(serde_json::json!({
        "logs": logs,
        "total": total,
        "limit": limit,
    }))
}

// Metrics endpoint
async fn get_metrics(state: State<Arc<ServerState>>) -> Json<serde_json::Value> {
    let routes_count = list_route_rows(&state.db)
        .await
        .map(|rows| rows.len())
        .unwrap_or(0);
    let providers_count = list_provider_rows(&state.db)
        .await
        .map(|rows| rows.len())
        .unwrap_or(0);
    Json(serde_json::json!({
        "routes_count": routes_count,
        "providers_count": providers_count,
        "probe_cache_size": state.probe_cache.len(),
    }))
}

#[cfg(test)]
mod tests {
    // Integration tests would go here
}
