use axum::extract::{ConnectInfo, Json, Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{Html, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::claude;
use crate::client::UpstreamClient;
use crate::config::{ModelPricing, ProxyConfig};
use crate::convert::response::openai_to_claude;
use crate::error::ProxyError;
use crate::history::{estimate_cost, CostSummary, PersistedRequestLog};
use crate::server::AppState;
use crate::stats::{RequestRecordDetail, StatsSnapshot};
use crate::types::openai::ResponseRequest;

#[derive(Deserialize)]
pub struct AdminConfigUpdate {
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub big_model: String,
    pub middle_model: Option<String>,
    pub small_model: String,
    pub anthropic_api_key: Option<String>,

    #[serde(default)]
    pub model_pricing: HashMap<String, ModelPricing>,
}

#[derive(Serialize)]
pub struct AdminStateResponse {
    pub config_path: String,
    pub config: AdminConfigView,
    pub stats: StatsSnapshot,
    pub history: HistoryView,
    pub claude: ClaudeStateView,
    pub connection: ConnectionInfoView,
}

#[derive(Serialize)]
pub struct AdminConfigView {
    pub host: String,
    pub port: u16,
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub big_model: String,
    pub middle_model: Option<String>,
    pub small_model: String,
    pub anthropic_api_key: Option<String>,

    pub model_pricing: HashMap<String, ModelPricing>,
}

#[derive(Serialize)]
pub struct HistoryView {
    pub request_log_count: usize,
    pub cost: CostSummary,
}

#[derive(Serialize)]
pub struct ClaudeStateView {
    pub settings_path: String,
    pub installed: bool,
    pub configured: bool,
}

#[derive(Serialize)]
pub struct ConnectionInfoView {
    pub proxy_url: String,
    pub auth_key: Option<String>,
    pub claude_command: String,
}

#[derive(Serialize)]
pub struct RequestDetailResponse {
    pub log: Option<PersistedRequestLog>,
    pub recent: Option<RequestRecordDetail>,
    pub estimated_cost_usd: Option<f64>,
    pub partial: bool,
}

#[derive(Serialize)]
pub struct ReplayResponse {
    pub request_id: String,
    pub replayed_at_epoch_ms: u64,
    pub forced_non_stream: bool,
    pub upstream_response: serde_json::Value,
    pub claude_response: serde_json::Value,
}

pub async fn require_loopback(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, axum::Json<serde_json::Value>)> {
    if addr.ip().is_loopback() {
        return Ok(next.run(request).await);
    }
    Err(forbidden_response())
}

pub async fn dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

pub async fn admin_state(State(state): State<Arc<AppState>>) -> Json<AdminStateResponse> {
    Json(build_admin_state(&state))
}

pub async fn update_config(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AdminConfigUpdate>,
) -> Result<Json<AdminStateResponse>, ProxyError> {
    let current = state.runtime.snapshot().config;
    let updated = apply_config_update(current, payload)?;
    persist_runtime_config(&state, updated)?;
    Ok(Json(build_admin_state(&state)))
}

pub async fn rotate_auth_key(
    State(state): State<Arc<AppState>>,
) -> Result<Json<AdminStateResponse>, ProxyError> {
    let mut config = state.runtime.snapshot().config;
    config.anthropic_api_key = Some(generate_auth_key());
    persist_runtime_config(&state, config)?;
    Ok(Json(build_admin_state(&state)))
}

pub async fn configure_claude_code(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ProxyError> {
    let config = state.runtime.snapshot().config;
    let auth_key = config
        .anthropic_api_key
        .clone()
        .ok_or_else(|| ProxyError::Config("ANTHROPIC_API_KEY is not configured".into()))?;
    claude::configure(config.port, &auth_key)?;
    Ok(Json(json!({
        "status": "ok",
        "message": "Claude Code settings.json updated",
        "settings_path": claude::settings_path().display().to_string(),
    })))
}

pub async fn request_detail(
    State(state): State<Arc<AppState>>,
    Path(request_id): Path<String>,
) -> Result<Json<RequestDetailResponse>, (StatusCode, axum::Json<serde_json::Value>)> {
    if let Some(log) = state.history.find(&request_id) {
        let pricing = state.runtime.snapshot().config.model_pricing;
        return Ok(Json(RequestDetailResponse {
            estimated_cost_usd: estimate_cost(&log, &pricing),
            log: Some(log),
            recent: None,
            partial: false,
        }));
    }

    if let Some(recent) = state.stats.find_request(&request_id) {
        return Ok(Json(RequestDetailResponse {
            log: None,
            recent: Some(recent),
            estimated_cost_usd: None,
            partial: true,
        }));
    }

    Err(not_found_response("Request log not found."))
}

pub async fn replay_request(
    State(state): State<Arc<AppState>>,
    Path(request_id): Path<String>,
) -> Result<Json<ReplayResponse>, (StatusCode, axum::Json<serde_json::Value>)> {
    let Some(log) = state.history.find(&request_id) else {
        return Err(not_found_response("Request log not found."));
    };
    let runtime = state.runtime.snapshot();
    let mut replay_request = serde_json::from_value::<ResponseRequest>(log.openai_request.clone())
        .map_err(internal_response)?;
    let forced_non_stream = replay_request.stream;
    if forced_non_stream {
        replay_request.stream = false;
    }
    let upstream = runtime
        .client
        .create_response(&replay_request, &runtime.config.openai_api_key)
        .await
        .map_err(proxy_response)?;
    let claude_response = openai_to_claude(
        &upstream,
        &log.original_model,
        log.usage.input_tokens + log.usage.cache_read_input_tokens.unwrap_or(0),
    );
    Ok(Json(ReplayResponse {
        request_id,
        replayed_at_epoch_ms: unix_epoch_ms(),
        forced_non_stream,
        upstream_response: serde_json::to_value(&upstream).map_err(internal_response)?,
        claude_response: serde_json::to_value(&claude_response).map_err(internal_response)?,
    }))
}

fn apply_config_update(
    mut current: ProxyConfig,
    payload: AdminConfigUpdate,
) -> Result<ProxyConfig, ProxyError> {
    validate_pricing(&payload.model_pricing)?;
    current.openai_base_url = require_non_empty(payload.openai_base_url, "OPENAI_BASE_URL")?;
    current.openai_api_key = require_non_empty(payload.openai_api_key, "OPENAI_API_KEY")?;
    current.big_model = require_non_empty(payload.big_model, "BIG_MODEL")?;
    current.small_model = require_non_empty(payload.small_model, "SMALL_MODEL")?;
    current.middle_model = normalize_optional(payload.middle_model);
    current.anthropic_api_key = normalize_optional(payload.anthropic_api_key);

    current.model_pricing = payload.model_pricing;
    Ok(current)
}

fn persist_runtime_config(state: &Arc<AppState>, config: ProxyConfig) -> Result<(), ProxyError> {
    UpstreamClient::new(&config)?;
    let path = ProxyConfig::default_config_path();
    config.save_to_file(&path)?;
    state.runtime.update_config(config)
}

fn build_admin_state(state: &Arc<AppState>) -> AdminStateResponse {
    let snapshot = state.runtime.snapshot();
    let config = snapshot.config;
    let proxy_url = format!("http://localhost:{}", config.port);
    let auth_key = config.anthropic_api_key.clone();
    let cost = state.history.cost_summary(&config.model_pricing);
    AdminStateResponse {
        config_path: ProxyConfig::default_config_path().display().to_string(),
        config: AdminConfigView {
            host: config.host.clone(),
            port: config.port,
            openai_base_url: config.openai_base_url.clone(),
            openai_api_key: config.openai_api_key.clone(),
            big_model: config.big_model.clone(),
            middle_model: config.middle_model.clone(),
            small_model: config.small_model.clone(),
            anthropic_api_key: auth_key.clone(),

            model_pricing: config.model_pricing.clone(),
        },
        stats: state.stats.snapshot(),
        history: HistoryView {
            request_log_count: state.history.total_log_count(),
            cost,
        },
        claude: ClaudeStateView {
            settings_path: claude::settings_path().display().to_string(),
            installed: claude::claude_code_installed(),
            configured: claude::is_configured(),
        },
        connection: ConnectionInfoView {
            proxy_url: proxy_url.clone(),
            auth_key: auth_key.clone(),
            claude_command: format!(
                "ANTHROPIC_BASE_URL={proxy_url} ANTHROPIC_API_KEY=\"{}\" ANTHROPIC_AUTH_TOKEN=\"\" claude",
                auth_key.unwrap_or_default()
            ),
        },
    }
}

fn require_non_empty(value: String, field_name: &str) -> Result<String, ProxyError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ProxyError::Config(format!("{field_name} cannot be empty")));
    }
    Ok(trimmed.to_string())
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn validate_pricing(pricing: &HashMap<String, ModelPricing>) -> Result<(), ProxyError> {
    for (model, price) in pricing {
        validate_cost(
            model,
            "input_cost_per_million",
            price.input_cost_per_million,
        )?;
        validate_cost(
            model,
            "output_cost_per_million",
            price.output_cost_per_million,
        )?;
        if let Some(cache_price) = price.cache_read_cost_per_million {
            validate_cost(model, "cache_read_cost_per_million", cache_price)?;
        }
    }
    Ok(())
}

fn validate_cost(model: &str, field: &str, value: f64) -> Result<(), ProxyError> {
    if value.is_finite() && value >= 0.0 {
        return Ok(());
    }
    Err(ProxyError::Config(format!(
        "Invalid pricing for {model}.{field}: expected a non-negative number"
    )))
}

fn forbidden_response() -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::FORBIDDEN,
        axum::Json(
            json!({"type":"error","error":{"type":"forbidden","message":"Dashboard is only available from localhost."}}),
        ),
    )
}

fn not_found_response(message: &str) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        axum::Json(json!({"type":"error","error":{"type":"not_found_error","message":message}})),
    )
}

fn internal_response(error: impl std::fmt::Display) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(
            json!({"type":"error","error":{"type":"api_error","message":error.to_string()}}),
        ),
    )
}

fn proxy_response(error: ProxyError) -> (StatusCode, axum::Json<serde_json::Value>) {
    (
        error.status_code(),
        axum::Json(
            json!({"type":"error","error":{"type":"api_error","message":error.message_text()}}),
        ),
    )
}

fn generate_auth_key() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:032x}")
}

fn unix_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

const DASHBOARD_HTML: &str = include_str!("dashboard.html");
