use axum::{
    extract::{DefaultBodyLimit, Json, State},
    http::Method,
    middleware,
    response::{sse::Sse, IntoResponse},
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::admin;
use crate::auth;
use crate::config::ProxyConfig;
use crate::convert;
use crate::error::ProxyError;
use crate::history::HistoryStore;
use crate::model_map;
use crate::request_log::build_request_log;
use crate::runtime::{RuntimeHandle, RuntimeSnapshot};
use crate::session::{SessionPlan, SessionStore};
use crate::stats::{RequestCompletion, RequestTicket, StatsCollector};
use crate::types::claude::MessagesRequest;
use crate::types::openai::{
    InputContentPart, ResponseInputItem, ResponseInputMessage, ResponseMessageContent,
    ResponseRequest,
};

#[derive(Clone)]
pub struct AppState {
    pub runtime: RuntimeHandle,
    pub stats: StatsCollector,
    pub history: HistoryStore,
    pub sessions: SessionStore,
}

pub fn create_router(state: AppState) -> Router {
    let shared_state = Arc::new(state);

    let api_routes = Router::new()
        .route("/v1/messages", post(create_message))
        .layer(middleware::from_fn_with_state(
            shared_state.clone(),
            auth::auth_middleware,
        ));

    let admin_routes = Router::new()
        .route("/dashboard", get(admin::dashboard))
        .route("/api/admin/state", get(admin::admin_state))
        .route("/api/admin/config", post(admin::update_config))
        .route("/api/admin/auth/rotate", post(admin::rotate_auth_key))
        .route(
            "/api/admin/claude/apply",
            post(admin::configure_claude_code),
        )
        .route(
            "/api/admin/requests/{request_id}",
            get(admin::request_detail),
        )
        .route(
            "/api/admin/requests/{request_id}/replay",
            post(admin::replay_request),
        )
        .layer(middleware::from_fn(admin::require_loopback));

    let public_routes = Router::new()
        .route("/health", get(health))
        .route("/test-connection", get(test_connection))
        .route("/", get(root));

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            let bytes = origin.as_bytes();
            bytes.starts_with(b"http://localhost")
                || bytes.starts_with(b"http://127.0.0.1")
                || bytes.starts_with(b"http://[::1]")
        }))
        .allow_methods([Method::POST, Method::GET, Method::OPTIONS])
        .allow_headers(tower_http::cors::Any);

    api_routes
        .merge(admin_routes)
        .merge(public_routes)
        .layer(cors)
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .with_state(shared_state)
}

pub async fn serve(config: ProxyConfig) -> Result<(), ProxyError> {
    let addr = format!("{}:{}", config.host, config.port);
    let runtime = RuntimeHandle::new(config)?;
    let (history, logs) = HistoryStore::load()?;
    let state = AppState {
        runtime,
        stats: StatsCollector::new(&logs),
        history,
        sessions: SessionStore::new(),
    };
    let app = create_router(state);

    tracing::info!("Proxy listening on {addr}");

    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to bind {addr}: {e}")))?;

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Shutdown signal received, draining connections...");
    };

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|e| ProxyError::Internal(format!("Server error: {e}")))?;

    Ok(())
}

async fn create_message(
    State(state): State<Arc<AppState>>,
    Json(request): Json<MessagesRequest>,
) -> Result<axum::response::Response, ProxyError> {
    let estimated_input_tokens = crate::token_count::count_request_tokens(&request);
    let stream = request.stream.unwrap_or(false);
    let runtime = state.runtime.snapshot();
    let (openai_request, session_plan) = prepare_openai_request(&state, &request, &runtime.config);
    let ticket =
        state
            .stats
            .begin_request(request.model.clone(), openai_request.model.clone(), stream);
    log_request(&request, &openai_request, estimated_input_tokens);

    if stream {
        return handle_streaming_message(
            state,
            runtime,
            request,
            openai_request,
            session_plan,
            estimated_input_tokens,
            ticket,
        )
        .await;
    }

    handle_non_streaming_message(
        runtime,
        state,
        request,
        openai_request,
        session_plan,
        estimated_input_tokens,
        ticket,
    )
    .await
}

async fn handle_streaming_message(
    state: Arc<AppState>,
    runtime: RuntimeSnapshot,
    request: MessagesRequest,
    openai_request: ResponseRequest,
    session_plan: Option<SessionPlan>,
    estimated_input_tokens: u32,
    ticket: RequestTicket,
) -> Result<axum::response::Response, ProxyError> {
    let first_byte_timeout = Duration::from_secs(runtime.config.streaming_first_byte_timeout);
    let idle_timeout = Duration::from_secs(runtime.config.streaming_idle_timeout);
    let request_snapshot = request.clone();
    let openai_snapshot = openai_request.clone();
    let event_stream = match runtime
        .client
        .create_response_stream(
            &openai_request,
            &runtime.config.openai_api_key,
            first_byte_timeout,
            idle_timeout,
        )
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            let completion = RequestCompletion::from_proxy_error(&error);
            persist_request_log(
                &state,
                &ticket,
                &completion,
                &request,
                &openai_request,
                None,
            );
            state.stats.finish(ticket, completion);
            return Err(error);
        }
    };

    let observer = build_stream_observer(
        state.clone(),
        ticket,
        request_snapshot,
        openai_snapshot,
        session_plan,
    );
    let claude_stream = convert::stream::openai_stream_to_claude(
        event_stream,
        request.model,
        idle_timeout,
        estimated_input_tokens,
        Some(observer),
    );

    Ok(Sse::new(claude_stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response())
}

async fn handle_non_streaming_message(
    runtime: RuntimeSnapshot,
    state: Arc<AppState>,
    request: MessagesRequest,
    openai_request: ResponseRequest,
    session_plan: Option<SessionPlan>,
    estimated_input_tokens: u32,
    ticket: RequestTicket,
) -> Result<axum::response::Response, ProxyError> {
    let openai_response = match runtime
        .client
        .create_response(&openai_request, &runtime.config.openai_api_key)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            let completion = RequestCompletion::from_proxy_error(&error);
            persist_request_log(
                &state,
                &ticket,
                &completion,
                &request,
                &openai_request,
                None,
            );
            state.stats.finish(ticket, completion);
            return Err(error);
        }
    };

    let claude_response = convert::response::openai_to_claude(
        &openai_response,
        &request.model,
        estimated_input_tokens,
    );
    let completion = RequestCompletion::success(
        200,
        claude_response.usage.clone(),
        claude_response.stop_reason.clone(),
    );
    commit_session_plan(
        &state,
        session_plan.as_ref(),
        &request,
        &openai_request.model,
        Some(&openai_response.id),
    );
    let response_payload = serde_json::to_value(&claude_response).ok();
    persist_request_log(
        &state,
        &ticket,
        &completion,
        &request,
        &openai_request,
        response_payload,
    );
    state.stats.finish(ticket, completion);
    Ok(Json(claude_response).into_response())
}

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let runtime = state.runtime.snapshot();
    Json(serde_json::json!({
        "status": "healthy",
        "timestamp": unix_now_secs().to_string(),
        "openai_api_configured": !runtime.config.openai_api_key.is_empty(),
        "client_api_key_validation": runtime.config.anthropic_api_key.is_some(),
    }))
}

async fn test_connection(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let runtime = state.runtime.snapshot();
    let test_req = ResponseRequest {
        model: runtime.config.small_model.clone(),
        input: vec![ResponseInputItem::Message(ResponseInputMessage {
            role: "user".into(),
            content: ResponseMessageContent::Parts(vec![InputContentPart::InputText {
                text: "Hello".into(),
            }]),
        })],
        max_output_tokens: 5,
        instructions: None,
        temperature: Some(0.0),
        top_p: None,
        stream: false,
        tools: None,
        tool_choice: None,
        reasoning: None,
        previous_response_id: None,
        prompt_cache_key: None,
        prompt_cache_retention: None,
    };

    match runtime
        .client
        .create_response(&test_req, &runtime.config.openai_api_key)
        .await
    {
        Ok(resp) => Json(serde_json::json!({
            "status": "success",
            "message": "Connected to upstream API",
            "model_used": runtime.config.small_model,
            "response_id": resp.id,
        }))
        .into_response(),
        Err(error) => Json(serde_json::json!({
            "status": "failed",
            "error": error.to_string(),
        }))
        .into_response(),
    }
}

async fn root(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let runtime = state.runtime.snapshot();
    Json(serde_json::json!({
        "message": format!("cc-proxy v{}", env!("CARGO_PKG_VERSION")),
        "status": "running",
        "config": {
            "openai_base_url": runtime.config.openai_base_url,
            "big_model": runtime.config.big_model,
            "middle_model": runtime.config.effective_middle_model(),
            "small_model": runtime.config.small_model,
        },
        "endpoints": {
            "messages": "/v1/messages",
            "health": "/health",
            "test_connection": "/test-connection",
            "dashboard": "/dashboard",
        }
    }))
}

fn build_stream_observer(
    state: Arc<AppState>,
    ticket: RequestTicket,
    request: MessagesRequest,
    openai_request: ResponseRequest,
    session_plan: Option<SessionPlan>,
) -> crate::convert::stream::StreamObserver {
    let shared_ticket = Arc::new(std::sync::Mutex::new(Some(ticket)));
    Arc::new(move |summary| {
        let mut guard = shared_ticket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(ticket) = guard.take() else {
            return;
        };
        let stop_reason = summary.stop_reason.clone();
        let completion = if summary.had_error {
            RequestCompletion::stream_error(
                summary.usage,
                stop_reason.clone(),
                summary.error_message.clone(),
            )
        } else {
            RequestCompletion::success(200, summary.usage, Some(stop_reason.clone()))
        };
        commit_session_plan(
            &state,
            session_plan.as_ref(),
            &request,
            &openai_request.model,
            summary.response_id.as_deref(),
        );
        let response_payload = Some(serde_json::json!({
            "stream_summary": {
                "stop_reason": stop_reason,
                "response_id": summary.response_id,
                "had_error": summary.had_error,
                "error_message": summary.error_message,
            }
        }));
        persist_request_log(
            &state,
            &ticket,
            &completion,
            &request,
            &openai_request,
            response_payload,
        );
        state.stats.finish(ticket, completion);
    })
}

fn prepare_openai_request(
    state: &Arc<AppState>,
    request: &MessagesRequest,
    config: &ProxyConfig,
) -> (ResponseRequest, Option<SessionPlan>) {
    let upstream_model = model_map::map_model(&request.model, config).model;
    let session_plan = config
        .supports_openai_responses_features()
        .then(|| state.sessions.plan(request, &upstream_model));
    let options = build_request_options(session_plan.as_ref(), config);
    let openai_request = convert::request::claude_to_openai_with_options(request, config, options);
    (openai_request, session_plan)
}

fn build_request_options(
    session_plan: Option<&SessionPlan>,
    config: &ProxyConfig,
) -> convert::request::RequestConversionOptions {
    convert::request::RequestConversionOptions {
        input_messages: session_plan.map(|plan| plan.input_messages.clone()),
        previous_response_id: session_plan.and_then(|plan| plan.previous_response_id.clone()),
        prompt_cache_key: session_plan.map(|plan| plan.session_key.clone()),
        prompt_cache_retention: config.prompt_cache_retention.clone(),
    }
}

fn commit_session_plan(
    state: &Arc<AppState>,
    session_plan: Option<&SessionPlan>,
    request: &MessagesRequest,
    upstream_model: &str,
    response_id: Option<&str>,
) {
    let (Some(plan), Some(response_id)) = (session_plan, response_id) else {
        return;
    };
    state
        .sessions
        .commit(plan, request, upstream_model, response_id);
}

fn persist_request_log(
    state: &Arc<AppState>,
    ticket: &RequestTicket,
    completion: &RequestCompletion,
    request: &MessagesRequest,
    openai_request: &ResponseRequest,
    response_payload: Option<serde_json::Value>,
) {
    match build_request_log(
        ticket,
        completion,
        request,
        openai_request,
        response_payload,
    ) {
        Ok(entry) => {
            if let Err(error) = state.history.append(entry) {
                tracing::warn!("Failed to persist request history: {error}");
            }
        }
        Err(error) => tracing::warn!("Failed to build request history: {error}"),
    }
}

fn log_request(
    request: &MessagesRequest,
    openai_request: &ResponseRequest,
    estimated_input_tokens: u32,
) {
    tracing::info!(
        model = %request.model,
        upstream_model = %openai_request.model,
        stream = ?request.stream,
        messages = request.messages.len(),
        tiktoken_input = estimated_input_tokens,
        max_tokens = request.max_tokens,
        "→ request"
    );
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
