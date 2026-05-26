//! Responses retrieval endpoints (`GET /v1/responses/{id}` and input items).

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::sync::Arc;

use crate::{
    error::error_response_to_response, relay::DownstreamOutputItem,
    runtime_config::ensure_operational_config_seeded, ServerState,
};
use modelwire_core::error::{Error, ErrorKind};
use modelwire_db::repo::responses::{get_items, get_response, ItemRecord};

#[derive(Debug, Serialize)]
struct RetrievedResponse {
    id: String,
    object: &'static str,
    created_at: i64,
    model: String,
    status: String,
    output: Vec<DownstreamOutputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<modelwire_core::ResponseUsage>,
}

#[derive(Debug, Serialize)]
struct InputItemsResponse {
    object: &'static str,
    data: Vec<serde_json::Value>,
}

pub async fn get_response_by_id(
    State(state): State<Arc<ServerState>>,
    Path(response_id): Path<String>,
) -> Response {
    if let Err(error) = ensure_operational_config_seeded(state.as_ref()).await {
        return error_response_to_response(error.to_response());
    }

    match load_response_payload(state.as_ref(), &response_id).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => error_response_to_response(error.to_response()),
    }
}

pub async fn get_response_input_items(
    State(state): State<Arc<ServerState>>,
    Path(response_id): Path<String>,
) -> Response {
    if let Err(error) = ensure_operational_config_seeded(state.as_ref()).await {
        return error_response_to_response(error.to_response());
    }

    if let Err(error) = ensure_response_exists(state.as_ref(), &response_id).await {
        return error_response_to_response(error.to_response());
    }

    let items = match get_items(&state.db, &response_id).await {
        Ok(items) => items,
        Err(error) => {
            return error_response_to_response(
                Error::new(
                    ErrorKind::InternalError,
                    format!("Failed to load response input items: {error}"),
                )
                .to_response(),
            );
        }
    };

    let data = items.into_iter().map(item_to_api_value).collect();
    Json(InputItemsResponse {
        object: "list",
        data,
    })
    .into_response()
}

async fn ensure_response_exists(state: &ServerState, response_id: &str) -> Result<(), Error> {
    let exists = get_response(&state.db, response_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to read response state: {error}"),
            )
        })?
        .is_some();
    if !exists {
        return Err(Error::new(
            ErrorKind::StateNotFound,
            format!("Response '{response_id}' was not found"),
        ));
    }
    Ok(())
}

async fn load_response_payload(
    state: &ServerState,
    response_id: &str,
) -> Result<RetrievedResponse, Error> {
    let response = get_response(&state.db, response_id)
        .await
        .map_err(|error| {
            Error::new(
                ErrorKind::InternalError,
                format!("Failed to read response state: {error}"),
            )
        })?
        .ok_or_else(|| {
            Error::new(
                ErrorKind::StateNotFound,
                format!("Response '{response_id}' was not found"),
            )
        })?;

    let items = get_items(&state.db, response_id).await.map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to load response items: {error}"),
        )
    })?;
    let output = items_to_output(items)?;
    let usage = response
        .usage_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<modelwire_core::ResponseUsage>(raw).ok());

    Ok(RetrievedResponse {
        id: response.id,
        object: "response",
        created_at: parse_created_ts(&response.created_at),
        model: response.downstream_model,
        status: response.status,
        output,
        usage,
    })
}

fn parse_created_ts(created_at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .map(|dt| dt.timestamp())
        .unwrap_or_else(|_| chrono::Utc::now().timestamp())
}

fn items_to_output(items: Vec<ItemRecord>) -> Result<Vec<DownstreamOutputItem>, Error> {
    let mut output = Vec::new();
    for item in items {
        match item.item_type.as_str() {
            "message" => {
                let blocks: Vec<serde_json::Value> = serde_json::from_str(&item.content_json)
                    .map_err(|error| {
                        Error::new(
                            ErrorKind::StateReplayFailed,
                            format!("Invalid message content in persisted state: {error}"),
                        )
                    })?;
                let content = blocks
                    .into_iter()
                    .filter_map(|block| {
                        let text = block.get("text").and_then(serde_json::Value::as_str)?;
                        Some(crate::relay::DownstreamContentBlock::OutputText {
                            text: text.to_string(),
                            annotations: Vec::new(),
                        })
                    })
                    .collect();
                output.push(DownstreamOutputItem::Message {
                    id: item.id,
                    status: "completed",
                    role: item.role.unwrap_or_else(|| "assistant".to_string()),
                    content,
                });
            }
            "function_call" => {
                let payload: serde_json::Value = serde_json::from_str(&item.content_json)
                    .unwrap_or_else(|_| serde_json::json!({}));
                output.push(DownstreamOutputItem::FunctionCall {
                    id: item.id,
                    call_id: item.call_id.unwrap_or_default(),
                    upstream_call_id: None,
                    name: payload
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: payload
                        .get("arguments")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("{}")
                        .to_string(),
                    status: "completed",
                });
            }
            "reasoning" => {
                let summary: Vec<serde_json::Value> =
                    serde_json::from_str(&item.content_json).unwrap_or_default();
                output.push(DownstreamOutputItem::Reasoning {
                    id: item.id,
                    summary,
                });
            }
            _ => {}
        }
    }
    Ok(output)
}

fn item_to_api_value(item: ItemRecord) -> serde_json::Value {
    match item.item_type.as_str() {
        "input_message" => serde_json::json!({
            "id": item.id,
            "type": "message",
            "role": item.role.unwrap_or_else(|| "user".to_string()),
            "content": serde_json::from_str::<serde_json::Value>(&item.content_json).unwrap_or_else(|_| serde_json::json!([])),
        }),
        "function_call_output" => {
            let payload: serde_json::Value =
                serde_json::from_str(&item.content_json).unwrap_or_else(|_| serde_json::json!({}));
            serde_json::json!({
                "id": item.id,
                "type": "function_call_output",
                "call_id": item.call_id.unwrap_or_default(),
                "output": payload.get("output").and_then(serde_json::Value::as_str).unwrap_or_default(),
            })
        }
        "input_function_call" => {
            let payload: serde_json::Value =
                serde_json::from_str(&item.content_json).unwrap_or_else(|_| serde_json::json!({}));
            serde_json::json!({
                "id": item.id,
                "type": "function_call",
                "call_id": item.call_id.unwrap_or_default(),
                "name": payload.get("name").and_then(serde_json::Value::as_str).unwrap_or_default(),
                "arguments": payload.get("arguments").and_then(serde_json::Value::as_str).unwrap_or("{}"),
            })
        }
        "message" => serde_json::json!({
            "id": item.id,
            "type": "message",
            "role": item.role.unwrap_or_else(|| "assistant".to_string()),
            "content": serde_json::from_str::<serde_json::Value>(&item.content_json).unwrap_or_else(|_| serde_json::json!([])),
        }),
        "function_call" => {
            let payload: serde_json::Value =
                serde_json::from_str(&item.content_json).unwrap_or_else(|_| serde_json::json!({}));
            serde_json::json!({
                "id": item.id,
                "type": "function_call",
                "call_id": item.call_id.unwrap_or_default(),
                "name": payload.get("name").and_then(serde_json::Value::as_str).unwrap_or_default(),
                "arguments": payload.get("arguments").and_then(serde_json::Value::as_str).unwrap_or("{}"),
            })
        }
        "reasoning" => serde_json::json!({
            "id": item.id,
            "type": "reasoning",
            "summary": serde_json::from_str::<serde_json::Value>(&item.content_json).unwrap_or_else(|_| serde_json::json!([])),
        }),
        _ => serde_json::json!({
            "id": item.id,
            "type": item.item_type,
            "content": item.content_json,
        }),
    }
}
