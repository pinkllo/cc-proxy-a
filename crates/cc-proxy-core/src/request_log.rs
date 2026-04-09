use serde::Serialize;
use serde_json::Value;

use crate::error::ProxyError;
use crate::history::PersistedRequestLog;
use crate::stats::{RequestCompletion, RequestTicket};

pub fn build_request_log(
    ticket: &RequestTicket,
    completion: &RequestCompletion,
    claude_request: &impl Serialize,
    openai_request: &impl Serialize,
    response_payload: Option<Value>,
) -> Result<PersistedRequestLog, ProxyError> {
    Ok(PersistedRequestLog {
        request_id: ticket.request_id.clone(),
        started_at_epoch_ms: ticket.started_at_epoch_ms,
        completed_at_epoch_ms: unix_epoch_ms(),
        latency_ms: ticket.started_at.elapsed().as_millis() as u64,
        original_model: ticket.model.clone(),
        upstream_model: ticket.upstream_model.clone(),
        stream: ticket.stream,
        success: completion.success,
        status: completion.http_status,
        upstream_status: completion.upstream_status,
        usage: completion.usage.clone(),
        stop_reason: completion.stop_reason.clone(),
        error_code: completion.error_code.clone(),
        error_message: completion.error_message.clone(),
        claude_request: serde_json::to_value(claude_request).map_err(|error| {
            ProxyError::Internal(format!("Failed to serialize Claude request: {error}"))
        })?,
        openai_request: serde_json::to_value(openai_request).map_err(|error| {
            ProxyError::Internal(format!("Failed to serialize OpenAI request: {error}"))
        })?,
        response_payload,
    })
}

fn unix_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
